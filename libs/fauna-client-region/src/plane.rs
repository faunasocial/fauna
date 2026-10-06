//! The app-side region content plane: the device's last-known-good store, the
//! verify-and-fold of relay replies, and what the render and the transparency
//! surface read (`region-blocking.md` § The content plane → *How an app obtains
//! its region's policy*, *The blocked render and the transparency surface*;
//! § Fail posture).
//!
//! # The fail posture, as code
//!
//! - **A failed fetch writes nothing.** [`RegionPlane::apply_reply`] is only
//!   ever handed a reply the nest actually gave; a transport failure never
//!   reaches it, and a refused envelope leaves the held one in place.
//! - **Replaced only by a newer verified document.** Every envelope goes through
//!   [`admit_artifact`] (inclusion, when the log serves it) and
//!   [`verify_artifact`] against this app's own registry and its own sequence
//!   floor. The nest is a relay, not a trust point.
//! - **The replay floor outlives the document and is keyed on the authority**
//!   (§ Fail posture, ratified 2026-08-20) — the same rule the nest keeps: a
//!   withdrawn or de-listed document never re-opens the replay window.
//! - **Staleness warns, never blocks, never relaxes.** [`RegionView::stale`] is
//!   a flag for the surface; nothing in the fold reads it.
//! - **Loaded at launch ahead of the first fetch**, and re-verified on load
//!   against the current registry, so a de-listed authority's document stops
//!   binding with the build that de-lists it.
//!
//! # Why the store is not the family plane's `SecretStore` slot
//!
//! The design pointed the device store at the seam the family plane's
//! supervision snapshot uses (`fauna-client-accounts`' `SecretStore`). That
//! seam is a platform credential store — a keyring entry — and a content-policy
//! envelope is bounded at 4 MiB (it may bundle a scorer), which no platform
//! keyring holds (Windows Credential Manager caps a blob at 2.5 KiB). It is also
//! the wrong class: the document is a **public, signed, per-device** fact, not
//! an account secret. So the at-rest *format* is owned here, one byte string
//! ([`RegionPlane::to_bytes`] / [`RegionPlane::load`]), and each platform shell
//! keeps it in its own install-scoped state (a file on the native apps) —
//! refuting that one claim of the design in the commit that builds it, as the
//! design section invites.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use fauna_core::content_category::ContentLabelEntry;
use fauna_core::region_authority::{
    AnchorState, ObjectId, PAYLOAD_KIND_CONTENT_POLICY, PolicyArtifact, RegionCode, RegionRegistry,
    STALE_AFTER_SECS, VerifiedArtifact, admit_artifact, compiled_in_witness_roster,
    verify_artifact,
};
use fauna_core::region_policy::{
    ContentPolicyDocument, GRAMMAR_VERSION, PolicyStatus, RegionRuleSet, rules_from_region_policy,
};
use fauna_core::scoring::LabelerPostInput;
use fauna_labeler::region::{PreparedScorer, scorers_in_use};
use fauna_protocol::region::RegionArtifactGetReply;
use serde::{Deserialize, Serialize};

use crate::source::{DeclaredRegion, RegionSource};

/// The at-rest record — this module is its single owner. dag-cbor, so an
/// envelope's bytes stay bytes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct StoredState {
    #[serde(default)]
    policies: Vec<StoredPolicy>,
    #[serde(default)]
    floors: Vec<SequenceFloor>,
    /// Hex of the last log head this device accepted (`None` in the pre-log era).
    #[serde(default)]
    log_head: Option<String>,
    /// Unix seconds of the last time this device's nest answered a relay ask.
    #[serde(default)]
    reached_at: Option<u64>,
    /// The last declaration this device's leaf made — the last-known-good a
    /// [`RegionPlane::new_pending`] plane stands on while an asynchronous leaf
    /// (a store's storefront) has not answered yet.
    #[serde(default)]
    declared: Option<StoredDeclared>,
}

/// The record's own form of a [`DeclaredRegion`] — the same map, its source
/// through the carrying [`StoredRegionSource`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredDeclared {
    code: RegionCode,
    source: StoredRegionSource,
}

impl From<&DeclaredRegion> for StoredDeclared {
    fn from(d: &DeclaredRegion) -> Self {
        Self {
            code: d.code.clone(),
            source: d.source.into(),
        }
    }
}

/// [`RegionSource`] as the device record holds it — the open arm of
/// `transport.md` § Rule 3 in full (*Open, carrying*). A newer build may
/// declare from a source this one cannot name, and the record is re-encoded on
/// every save, so the name is held exactly as read ([`Self::Other`]) and
/// re-emitted unchanged. It never reaches a view: [`RegionPlane::load`] keeps
/// such a declaration aside, enforcing its region, and the surface names no
/// source for it (`RegionSource` stays the closed type every app renders).
///
/// The named variants mirror [`RegionSource`] one for one, and the two
/// conversions below match exhaustively, so a source added to the view type
/// does not compile until the record can store it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredRegionSource {
    Storefront,
    SystemRegion,
    SystemLocale,
    BrowserLocale,
    #[serde(untagged)]
    Other(String),
}

impl From<RegionSource> for StoredRegionSource {
    fn from(source: RegionSource) -> Self {
        match source {
            RegionSource::Storefront => Self::Storefront,
            RegionSource::SystemRegion => Self::SystemRegion,
            RegionSource::SystemLocale => Self::SystemLocale,
            RegionSource::BrowserLocale => Self::BrowserLocale,
        }
    }
}

impl StoredRegionSource {
    /// The view type's source, or `None` for one this build cannot name.
    fn known(&self) -> Option<RegionSource> {
        match self {
            Self::Storefront => Some(RegionSource::Storefront),
            Self::SystemRegion => Some(RegionSource::SystemRegion),
            Self::SystemLocale => Some(RegionSource::SystemLocale),
            Self::BrowserLocale => Some(RegionSource::BrowserLocale),
            Self::Other(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredPolicy {
    envelope: PolicyArtifact,
    #[serde(default)]
    last_checked_at: Option<u64>,
    #[serde(default)]
    relay_stale: bool,
}

/// The highest sequence ever accepted from one authority for one region —
/// stored apart from the document so retiring the document never lowers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SequenceFloor {
    region: RegionCode,
    authority_name: String,
    sequence: u64,
}

/// Whether one held document binds, as the transparency surface says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyState {
    /// Every rule applies.
    Applied,
    /// A grammar version this build does not implement — inert, and the surface
    /// says so (§ The policy document).
    Inert { version: u32 },
    /// At a version this build implements, but not a well-formed document.
    /// Nothing applies.
    Malformed(String),
}

/// One policy on the chain, as the transparency surface lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRow {
    pub region: RegionCode,
    /// As the registry names it — never as the artifact names itself.
    pub authority_name: String,
    pub sequence: u64,
    pub issued_at: u64,
    pub state: PolicyState,
}

/// What the settings region surface renders.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RegionView {
    /// `None` when the platform declares no region (the surface says so).
    pub declared: Option<DeclaredRegion>,
    /// Every held policy on the declared region's chain, most specific first.
    pub policies: Vec<PolicyRow>,
    /// Unix seconds of the most recent time the log answered for any policy
    /// on the chain (the relay's own report), else of this device's last
    /// answer from its nest.
    pub last_checked_at: Option<u64>,
    /// The warning — never an outage (§ Fail posture).
    pub stale: bool,
}

/// One document held in force (verified, decoded, attributed).
struct Held {
    verified: VerifiedArtifact,
    state: PolicyState,
    rule_set: RegionRuleSet,
    scorers: Vec<PreparedScorer>,
    last_checked_at: Option<u64>,
    relay_stale: bool,
}

/// The whole app-side plane for one device.
pub struct RegionPlane {
    declared: Option<DeclaredRegion>,
    /// A recorded declaration whose source this build cannot name, which a
    /// pending plane stands on ([`Self::load`]): its region is enforced like
    /// any declaration's, the view names none, and [`Self::to_bytes`]
    /// re-emits it unchanged until the leaf answers.
    carried_declaration: Option<StoredDeclared>,
    /// The stored record did not decode ([`Self::load`]): this build holds
    /// nothing from it and [`Self::to_bytes`] writes nothing over it.
    record_unreadable: bool,
    /// The leaf has not answered yet ([`Self::new_pending`]): [`Self::load`]
    /// takes the recorded declaration, and [`Self::redeclare`] ends it.
    pending: bool,
    registry: RegionRegistry,
    /// Keyed by region code.
    held: BTreeMap<RegionCode, Held>,
    floors: Vec<SequenceFloor>,
    log_head: Option<ObjectId>,
    reached_at: Option<u64>,
    /// Scorer answers per item, cleared whenever what is held changes. A module
    /// is deterministic, so an answer for an item never changes under one set
    /// of documents; the render asks on every paint.
    label_cache: RefCell<HashMap<[u8; 32], Vec<ContentLabelEntry>>>,
}

impl RegionPlane {
    /// A plane with nothing held — the fresh-subject arm of § Fail posture: the
    /// render is unblocked until the first successful fetch.
    pub fn new(declared: Option<DeclaredRegion>, registry: RegionRegistry) -> Self {
        Self {
            declared,
            carried_declaration: None,
            record_unreadable: false,
            pending: false,
            registry,
            held: BTreeMap::new(),
            floors: Vec::new(),
            log_head: None,
            reached_at: None,
            label_cache: RefCell::new(HashMap::new()),
        }
    }

    /// A plane whose leaf answers **asynchronously** (a store build's
    /// storefront): until [`Self::redeclare`] hands it the leaf's answer, it
    /// stands on the declaration the device record carries ([`Self::load`]) —
    /// § Fail posture's "enforcement never moves on no information", applied to
    /// the declaration itself. A device with no record declares nothing until
    /// the leaf answers (the fresh-subject residual).
    pub fn new_pending(registry: RegionRegistry) -> Self {
        Self {
            pending: true,
            ..Self::new(None, registry)
        }
    }

    /// Install the leaf's answer, ending a pending plane's last-known-good
    /// declaration. Returns whether the declaration changed — the record must
    /// be persisted and the render re-read.
    pub fn redeclare(&mut self, declared: Option<DeclaredRegion>) -> bool {
        self.pending = false;
        if self.declared == declared && self.carried_declaration.is_none() {
            return false;
        }
        self.declared = declared;
        self.carried_declaration = None;
        self.label_cache.borrow_mut().clear();
        true
    }

    pub fn declared(&self) -> Option<&DeclaredRegion> {
        self.declared.as_ref()
    }

    /// The declared region's ancestor chain, most specific first — every region
    /// whose policy this device applies (§ Regions compose along the registry's
    /// parent chain). Empty when nothing is declared or the declared region is
    /// not enrolled; a chain the registry cannot resolve (a cycle, too deep) is
    /// loud in the log and applies nothing rather than some prefix of it.
    pub fn chain(&self) -> Vec<RegionCode> {
        let Some(code) = self
            .declared
            .as_ref()
            .map(|d| &d.code)
            .or(self.carried_declaration.as_ref().map(|d| &d.code))
        else {
            return Vec::new();
        };
        if self.registry.region(code).is_none() {
            return Vec::new();
        }
        self.registry
            .chain(code)
            .map_err(|e| tracing::warn!("region: the declared region's chain: {e}"))
            .unwrap_or_default()
    }

    /// Restore the at-rest record (`None`: nothing was ever stored) — call at
    /// launch, **ahead of** the first fetch.
    ///
    /// Every stored envelope is re-verified against the current registry, so a
    /// document whose authority this build no longer enrols stops binding here.
    ///
    /// An unreadable record — corrupt, or written by a newer build in a shape
    /// this one cannot read — is `no information` for enforcement: nothing is
    /// held, exactly where a fresh install is. But it is **never written over**:
    /// its replay floors are the rollback evidence the record exists to keep, so
    /// [`Self::to_bytes`] writes nothing for the rest of this plane's life and
    /// the bytes stay as they are for a build that can read them
    /// (`transport.md` § Rule 3 in full → *The store around the enum*).
    ///
    /// A pending plane's recorded declaration whose source this build cannot
    /// name ([`StoredRegionSource::Other`]) still binds — its region is
    /// enforced, never relaxed — while the view names no source for it.
    pub fn load(&mut self, bytes: Option<&[u8]>, now: u64) {
        let Some(bytes) = bytes else { return };
        let stored: StoredState = match fauna_protocol::decode_strict(bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "region: the stored record does not decode; holding nothing from it and \
                     never writing over it: {e:?}"
                );
                self.record_unreadable = true;
                return;
            }
        };
        if self.pending
            && self.declared.is_none()
            && let Some(stored) = stored.declared
        {
            match stored.source.known() {
                Some(source) => {
                    self.declared = Some(DeclaredRegion {
                        code: stored.code,
                        source,
                    });
                }
                None => self.carried_declaration = Some(stored),
            }
        }
        self.floors = stored.floors;
        self.log_head = stored.log_head.as_deref().and_then(ObjectId::parse_hex);
        self.reached_at = stored.reached_at;
        for policy in stored.policies {
            let region = policy.envelope.region.clone();
            match verify_artifact(policy.envelope, &self.registry, now, None) {
                Ok(verified) => {
                    let held = hold(verified, policy.last_checked_at, policy.relay_stale);
                    self.held.insert(region, held);
                }
                Err(e) => tracing::info!(
                    "region: a stored {region} document no longer verifies, retiring it: {e}"
                ),
            }
        }
        self.label_cache.borrow_mut().clear();
    }

    /// The at-rest record to persist after [`Self::apply_reply`] reports a
    /// change — **empty when the stored record did not decode** ([`Self::load`]),
    /// which every shell's write treats as "write nothing"
    /// ([`crate::store::write_record`]; the web shell's refresh answers `null`).
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.record_unreadable {
            return Vec::new();
        }
        let stored = StoredState {
            policies: self
                .held
                .values()
                .map(|h| StoredPolicy {
                    envelope: h.verified.artifact().clone(),
                    last_checked_at: h.last_checked_at,
                    relay_stale: h.relay_stale,
                })
                .collect(),
            floors: self.floors.clone(),
            log_head: self.log_head.map(|h| h.to_hex()),
            reached_at: self.reached_at,
            declared: self
                .declared
                .as_ref()
                .map(StoredDeclared::from)
                .or_else(|| self.carried_declaration.clone()),
        };
        fauna_protocol::encode_canonical(&stored)
            .map(|b| b.to_vec())
            .unwrap_or_else(|e| {
                // Unreachable for this float-free shape; persisting nothing is
                // the `no information` direction.
                tracing::warn!("region: cannot encode the snapshot: {e:?}");
                Vec::new()
            })
    }

    /// Fold one relay reply for `region` (asked at `now`). Returns whether the
    /// at-rest record changed and should be persisted.
    ///
    /// A reply with no envelope is "no document" — the fresh-subject answer, or
    /// a nest that has not reached the log yet — and **keeps** whatever this
    /// device already holds: a relay that lost its cache must not be able to
    /// relax a device.
    pub fn apply_reply(
        &mut self,
        region: &RegionCode,
        reply: RegionArtifactGetReply,
        now: u64,
    ) -> bool {
        self.reached_at = Some(now);
        let Some(envelope) = reply.envelope else {
            if let Some(held) = self.held.get_mut(region) {
                held.last_checked_at = reply.last_checked_at.or(held.last_checked_at);
                held.relay_stale = reply.stale;
            }
            return true;
        };
        if envelope.region != *region || envelope.payload_kind != PAYLOAD_KIND_CONTENT_POLICY {
            tracing::warn!(
                "region: the relay answered {region}/{PAYLOAD_KIND_CONTENT_POLICY} with an \
                 envelope for {}/{} — refusing it",
                envelope.region,
                envelope.payload_kind
            );
            return true;
        }
        // The same envelope again: nothing to verify, only the clock moves.
        if let Some(held) = self.held.get_mut(region)
            && *held.verified.artifact() == envelope
        {
            held.last_checked_at = reply.last_checked_at;
            held.relay_stale = reply.stale;
            return true;
        }
        let anchor = AnchorState::with_last_accepted(self.log_head);
        let anchor = match admit_artifact(
            &envelope,
            reply.evidence.as_ref(),
            &anchor,
            &compiled_in_witness_roster(),
        ) {
            Ok(anchor) => anchor,
            Err(e) => {
                tracing::warn!("region: the relayed {region} envelope's inclusion is refused: {e}");
                return true;
            }
        };
        let floor = self
            .registry
            .region(region)
            .and_then(|entry| self.floor(region, &entry.authority_name));
        let verified = match verify_artifact(envelope, &self.registry, now, floor) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("region: the relayed {region} envelope is refused: {e}");
                return true;
            }
        };
        self.raise_floor(region, verified.authority_name(), verified.sequence());
        self.log_head = anchor.last_accepted;
        let held = hold(verified, reply.last_checked_at, reply.stale);
        self.held.insert(region.clone(), held);
        self.label_cache.borrow_mut().clear();
        true
    }

    /// Fold every answer [`crate::fetch_chain`] returned (asked at `now`).
    /// A failed ask is dropped — a failed fetch writes nothing (§ Fail
    /// posture). Returns whether the at-rest record changed: the shell then
    /// persists [`Self::to_bytes`] and re-arms its render engine with
    /// [`Self::rule_sets`].
    pub fn apply_replies(
        &mut self,
        replies: Vec<(RegionCode, Result<RegionArtifactGetReply, String>)>,
        now: u64,
    ) -> bool {
        let mut changed = false;
        for (region, reply) in replies {
            match reply {
                Ok(reply) => changed |= self.apply_reply(&region, reply, now),
                Err(e) => tracing::info!("region: the relay ask for {region} failed: {e}"),
            }
        }
        changed
    }

    fn floor(&self, region: &RegionCode, authority_name: &str) -> Option<u64> {
        self.floors
            .iter()
            .find(|f| f.region == *region && f.authority_name == authority_name)
            .map(|f| f.sequence)
    }

    fn raise_floor(&mut self, region: &RegionCode, authority_name: &str, sequence: u64) {
        match self
            .floors
            .iter_mut()
            .find(|f| f.region == *region && f.authority_name == authority_name)
        {
            Some(f) => f.sequence = f.sequence.max(sequence),
            None => self.floors.push(SequenceFloor {
                region: region.clone(),
                authority_name: authority_name.to_string(),
                sequence,
            }),
        }
    }

    /// The held documents on the declared chain, most specific first.
    fn on_chain(&self) -> impl Iterator<Item = &Held> {
        self.chain()
            .into_iter()
            .filter_map(|region| self.held.get(&region))
    }

    /// The rule sets the composed render call folds — the argument
    /// `fauna_core::obligation::ContentPolicyState::set_region_policies` takes,
    /// most specific first.
    pub fn rule_sets(&self) -> Vec<RegionRuleSet> {
        self.on_chain().map(|h| h.rule_set.clone()).collect()
    }

    /// Whether any document on the chain bundles a scorer a rule reads — the
    /// render's fast path skips [`Self::labels_for`] when none does.
    pub fn has_scorers(&self) -> bool {
        self.on_chain().any(|h| !h.scorers.is_empty())
    }

    /// The factors the chain's bundled scorers produce for one item, to join the
    /// item's own labels before the fold (§ The policy document, `scorers`).
    ///
    /// `content_id` is the item's 32-byte id (what a `list` scorer is keyed by;
    /// `None` for an item without one, which no list can name); `input` is what a
    /// `wasm` scorer reads, post-decrypt, on-device. A scorer that says nothing,
    /// or fails, adds no factor.
    pub fn labels_for(
        &self,
        content_id: Option<&[u8; 32]>,
        input: &LabelerPostInput,
    ) -> Vec<ContentLabelEntry> {
        if !self.has_scorers() {
            return Vec::new();
        }
        if let Some(id) = content_id
            && let Some(cached) = self.label_cache.borrow().get(id)
        {
            return cached.clone();
        }
        let no_id = [0u8; 32];
        let key = content_id.unwrap_or(&no_id);
        let labels: Vec<ContentLabelEntry> = self
            .on_chain()
            .flat_map(|h| h.scorers.iter())
            .filter_map(|scorer| {
                // An item with no id is only ever scored by a module: a list
                // looked up under the all-zero key would name nothing real.
                match scorer.score(key, input) {
                    Ok(Some(permille)) => Some(ContentLabelEntry {
                        category: scorer.factor.clone(),
                        confidence_per_mille: permille,
                    }),
                    Ok(None) => None,
                    Err(e) => {
                        tracing::debug!("region: scorer {} says nothing: {e:#}", scorer.factor);
                        None
                    }
                }
            })
            .collect();
        if let Some(id) = content_id {
            self.label_cache.borrow_mut().insert(*id, labels.clone());
        }
        labels
    }

    /// An item's labels with the chain's scorer factors joined — the input the
    /// composed verdict folds. Borrows the item's own labels when no scorer is
    /// in force (every device today), without building `input`.
    pub fn join_labels<'a>(
        &self,
        content_id: Option<&[u8; 32]>,
        labels: &'a [ContentLabelEntry],
        input: impl FnOnce() -> LabelerPostInput,
    ) -> Cow<'a, [ContentLabelEntry]> {
        if !self.has_scorers() {
            return Cow::Borrowed(labels);
        }
        let extra = self.labels_for(content_id, &input());
        if extra.is_empty() {
            return Cow::Borrowed(labels);
        }
        let mut joined = labels.to_vec();
        joined.extend(extra);
        Cow::Owned(joined)
    }

    /// What the settings region surface renders.
    pub fn view(&self, now: u64) -> RegionView {
        let policies: Vec<PolicyRow> = self
            .on_chain()
            .map(|h| PolicyRow {
                region: h.verified.region().clone(),
                authority_name: h.verified.authority_name().to_string(),
                sequence: h.verified.sequence(),
                issued_at: h.verified.issued_at(),
                state: h.state.clone(),
            })
            .collect();
        let log_checked = self.on_chain().filter_map(|h| h.last_checked_at).max();
        let relay_stale = self.on_chain().any(|h| h.relay_stale);
        // This device's own clock: a device that has not reached its nest for
        // longer than the shared staleness bound warns too — once the plane is
        // live at all (a declared, enrolled region).
        let device_stale = !self.chain().is_empty()
            && self
                .reached_at
                .is_none_or(|t| now.saturating_sub(t) > STALE_AFTER_SECS)
            && !self.held.is_empty();
        RegionView {
            declared: self.declared.clone(),
            policies,
            last_checked_at: log_checked.or(self.reached_at),
            stale: relay_stale || device_stale,
        }
    }
}

/// Decode, attribute and prepare one verified document.
fn hold(verified: VerifiedArtifact, last_checked_at: Option<u64>, relay_stale: bool) -> Held {
    let region = verified.region().clone();
    let authority = verified.authority_name().to_string();
    let (state, rule_set, scorers) = match verified.content_policy() {
        Ok(document) => {
            let rule_set = rules_from_region_policy(&document, &region, &authority);
            let state = match &rule_set.status {
                PolicyStatus::Applied => PolicyState::Applied,
                PolicyStatus::InertUnimplementedVersion { version } => {
                    PolicyState::Inert { version: *version }
                }
                PolicyStatus::Malformed(defect) => PolicyState::Malformed(defect.to_string()),
            };
            let scorers = prepare_scorers(&document, &region);
            (state, rule_set, scorers)
        }
        Err(e) => {
            // Version first, as `ContentPolicyDocument::status` reads it: a newer
            // grammar whose shapes this build cannot decode is inert, never
            // "malformed".
            let state = match peek_version(verified.artifact()) {
                Some(version) if version != GRAMMAR_VERSION => PolicyState::Inert { version },
                _ => PolicyState::Malformed(e.to_string()),
            };
            let status = match &state {
                PolicyState::Inert { version } => {
                    PolicyStatus::InertUnimplementedVersion { version: *version }
                }
                // No `PolicyDefect` names an undecodable payload, and the rule
                // set's status is read only for "does it apply": an inert status
                // with version 0 applies nothing, and the surface reads `state`.
                _ => PolicyStatus::InertUnimplementedVersion { version: 0 },
            };
            let rule_set = RegionRuleSet {
                region: region.clone(),
                authority_name: authority,
                status,
                rules: Vec::new(),
            };
            (state, rule_set, Vec::new())
        }
    };
    Held {
        verified,
        state,
        rule_set,
        scorers,
        last_checked_at,
        relay_stale,
    }
}

fn prepare_scorers(document: &ContentPolicyDocument, region: &RegionCode) -> Vec<PreparedScorer> {
    scorers_in_use(document, region)
        .into_iter()
        .filter_map(|scorer| {
            PreparedScorer::prepare(region, scorer)
                .map_err(|e| tracing::warn!("region: a bundled scorer does not prepare: {e:#}"))
                .ok()
        })
        .collect()
}

/// The `version` of a payload that does not decode as this build's document.
fn peek_version(artifact: &PolicyArtifact) -> Option<u32> {
    #[derive(Deserialize)]
    struct VersionOnly {
        version: u32,
        #[serde(flatten)]
        _rest: BTreeMap<String, fauna_protocol::Value>,
    }
    fauna_protocol::decode_strict::<VersionOnly>(&artifact.payload)
        .ok()
        .map(|v| v.version)
}
