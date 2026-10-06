//! UniFFI façade for the controversial-class feature plane's transparency read
//! (`fauna.features.status` — `docs/goal/architecture/dynamic-features.md`
//! § Transparency & auditability).
//!
//! [`FfiFeaturesClient`] wraps `fauna_client_features::FeaturesClient` and
//! hands the native apps the **already-derived** rows rather than the raw
//! wire reply: the join, the `remaining = limit − observed` arithmetic, the
//! hide-or-disable decision *and the string vocabulary* all live in the shared
//! crate's [`fauna_client_features::row`], so this module is a mechanical
//! mirror with no judgement of its own (priorities #1/#2). Linux and tui call
//! `feature_rows` directly; the web SPA reaches the same rows through the wasm
//! twin (`libs/fauna-wasm/src/features.rs`). All four therefore render the same
//! words for the same state.
//!
//! Gated behind `value-format` (default-on, dropped from the Go mail-bridge
//! `--no-default-features` build) for the same reason as `family.rs`'s picker
//! catalogs: a `#[uniffi::export]` returning a `fauna_core` type
//! ([`LocalizedText`] here) makes uniffi-bindgen-go emit an uncompilable bare
//! `import "fauna_core"`. The mail bridge has no feature-limits surface.
//!
//! Deliberately **not** gated on `payments`/`zaps`: the plane gates three
//! members, so its read answers for whichever ones a build ships — the same
//! reason the nest's handler is ungated.

use std::sync::{Arc, Mutex};

use fauna_client::NestClient;
use fauna_client_features::{
    AuthoredRow, AuthoringTier, CellMagnitudes, EditorCell, FeatureRow, FeaturesClient,
    PolicyEditor, PolicyEditorView, RowCell, SaveError, feature_rows,
};
use fauna_core::localized::LocalizedText;

use crate::{FfiError, stringify};

/// FFI mirror of [`fauna_client_features::RowCell`] — one bound in force, what
/// has been spent against it, and **which tier set it** (the attribution
/// boundary 4 requires: "no silent gates" means the person bound can see both
/// the restriction and its source).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiLimitCell {
    /// `operations` | `counterparties` | `volume`.
    pub dimension: String,
    /// `day` | `week` | `month` — trailing, not calendar: "week" is the last 7
    /// day buckets (§ The quota grammar).
    pub window: String,
    pub limit: u64,
    pub observed: u64,
    /// `limit − observed`, floored at zero.
    pub remaining: u64,
    /// `structural` | `region` | `admin` | `guardian` | `self`.
    pub tier: String,
    /// The tier's human name, for a row's "set by …" half.
    pub tier_label: LocalizedText,
    pub exhausted: bool,
    /// What this cell counts, over which window — *"Uses per week"*. Both
    /// substitutions are themselves i18n keys: resolve them before substituting.
    pub label: LocalizedText,
    /// The headroom sentence. `{remaining}` / `{limit}` are finished numbers
    /// **unless** [`Self::magnitudes`] is set, in which case the app resolves
    /// those and substitutes them instead — the `BackupLastUploadDisplay`
    /// two-level shape, for the same reason (a `LocalizedText` argument is a
    /// flat string, so a localized magnitude cannot be nested inside one).
    pub value: LocalizedText,
    /// Set exactly for `volume` cells, whose magnitudes carry a unit.
    pub magnitudes: Option<FfiCellMagnitudes>,
}

/// FFI mirror of [`fauna_client_features::CellMagnitudes`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiCellMagnitudes {
    pub remaining: LocalizedText,
    pub limit: LocalizedText,
}

impl From<CellMagnitudes> for FfiCellMagnitudes {
    fn from(m: CellMagnitudes) -> Self {
        let CellMagnitudes { remaining, limit } = m;
        FfiCellMagnitudes { remaining, limit }
    }
}

impl From<RowCell> for FfiLimitCell {
    fn from(c: RowCell) -> Self {
        let RowCell {
            dimension,
            window,
            limit,
            observed,
            remaining,
            tier,
            tier_label,
            exhausted,
            label,
            value,
            magnitudes,
        } = c;
        FfiLimitCell {
            dimension,
            window,
            limit,
            observed,
            remaining,
            tier,
            tier_label,
            exhausted,
            label,
            value,
            magnitudes: magnitudes.map(FfiCellMagnitudes::from),
        }
    }
}

/// FFI mirror of [`fauna_client_features::FeatureRow`] — one registry member's
/// row on a feature-limits surface.
///
/// Destructured field-by-field in the `From` impl below, so a new field in the
/// shared row is a compile error here rather than a silently missing column on
/// four apps (the mirror-drift guard the rest of this crate uses).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFeatureRow {
    /// The stable key — `payments` | `zaps` | `p2p-share`.
    pub feature: String,
    /// The member's human name; the stable key is a wire/cargo identifier and
    /// never reaches a screen.
    pub name: LocalizedText,
    /// `allow` | `deny` | `limit`, derived from the bounds actually in force so
    /// it can never disagree with them.
    pub availability: String,
    pub denied_by: Option<String>,
    /// Every bound in force, in a stable (dimension, window) order.
    pub cells: Vec<FfiLimitCell>,
    /// A per-operation magnitude cap. Not a quota — it bounds one operation's
    /// size rather than a window's total, so it never runs out.
    pub per_operation_max: Option<u64>,
    pub per_operation_max_tier: Option<String>,
    /// `bytes` | `millisats` — so a magnitude is never rendered in the wrong
    /// unit.
    pub unit: String,
    /// `available` | `disabled` | `hidden` — the Dim-3 decision for this
    /// member's affordances.
    pub affordance: String,
    /// Why the affordance is disabled, when it is. Its `{window}` substitution
    /// is an i18n key — resolve it before substituting.
    pub restriction: Option<LocalizedText>,
    /// The row's one-word state for a status column — *Available* /
    /// *Restricted* — derived from [`Self::affordance`] so it can never
    /// disagree with whether the affordance works.
    pub status: LocalizedText,
}

impl From<FeatureRow> for FfiFeatureRow {
    fn from(r: FeatureRow) -> Self {
        let FeatureRow {
            feature,
            name,
            availability,
            denied_by,
            cells,
            per_operation_max,
            per_operation_max_tier,
            unit,
            affordance,
            restriction,
            status,
        } = r;
        FfiFeatureRow {
            feature,
            name,
            availability,
            denied_by,
            cells: cells.into_iter().map(FfiLimitCell::from).collect(),
            per_operation_max,
            per_operation_max_tier,
            unit,
            affordance,
            restriction,
            status,
        }
    }
}

// ── The authoring half (dynamic-features.md § Authoring surfaces) ─────

/// FFI mirror of [`fauna_client_features::AuthoredRow`] — one member's row on
/// an authoring host: its name and the tier's AUTHORED document in words.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiAuthoredRow {
    /// The stable key — `payments` | `zaps` | `p2p-share`.
    pub feature: String,
    pub name: LocalizedText,
    /// The tier's authored document in words (`admin-nest-feature-limits-summary`
    /// / `feature-limits-own-summary`).
    pub summary: LocalizedText,
    pub has_document: bool,
    pub unreadable: bool,
}

impl From<AuthoredRow> for FfiAuthoredRow {
    fn from(r: AuthoredRow) -> Self {
        let AuthoredRow {
            feature,
            name,
            summary,
            has_document,
            unreadable,
        } = r;
        FfiAuthoredRow {
            feature,
            name,
            summary,
            has_document,
            unreadable,
        }
    }
}

/// FFI mirror of [`fauna_client_features::EditorCell`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiEditorCell {
    /// What the cell bounds. Its substitutions are i18n keys — resolve them
    /// before substituting.
    pub label: LocalizedText,
    /// The typed text, verbatim.
    pub text: String,
    /// The no-effect note. `{limit}` is a finished number **unless**
    /// `note_magnitude` is set, in which case resolve that and substitute it
    /// (the `FfiCellMagnitudes` two-level shape).
    pub note: Option<LocalizedText>,
    pub note_magnitude: Option<LocalizedText>,
}

impl From<EditorCell> for FfiEditorCell {
    fn from(c: EditorCell) -> Self {
        let EditorCell {
            label,
            text,
            note,
            note_magnitude,
        } = c;
        FfiEditorCell {
            label,
            text,
            note,
            note_magnitude,
        }
    }
}

/// FFI mirror of [`fauna_client_features::PolicyEditorView`] — everything the
/// shared `feature-policy-editor` paints.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiPolicyEditorView {
    pub feature: String,
    /// `admin` | `guardian` | `self`.
    pub tier: String,
    /// Its `{feature}` is an i18n key — resolve it before substituting.
    pub title: LocalizedText,
    pub on: bool,
    /// One per editable slot while On; empty while Off.
    pub cells: Vec<FfiEditorCell>,
    pub off_note: Option<LocalizedText>,
    /// Whether `feature-policy-editor-remove-button` renders.
    pub can_remove: bool,
    pub hint: LocalizedText,
}

impl From<PolicyEditorView> for FfiPolicyEditorView {
    fn from(v: PolicyEditorView) -> Self {
        let PolicyEditorView {
            feature,
            tier,
            title,
            on,
            cells,
            off_note,
            can_remove,
            hint,
        } = v;
        FfiPolicyEditorView {
            feature,
            tier,
            title,
            on,
            cells: cells.into_iter().map(FfiEditorCell::from).collect(),
            off_note,
            can_remove,
            hint,
        }
    }
}

/// A save's or a removal's verdict. Exactly one field is set: `status` (the
/// `feature-policy-editor-status` line — the write landed and the editor has
/// re-seeded from a fresh read) or `invalid` (a cell the shared parser refused —
/// nothing was dispatched; render it in the page's `error-message`). A nest
/// refusal or a transport failure is the call's `FfiError` instead.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiEditorVerdict {
    pub status: Option<LocalizedText>,
    pub invalid: Option<LocalizedText>,
}

fn tier_of(key: &str) -> Result<AuthoringTier, FfiError> {
    AuthoringTier::from_key(key).ok_or_else(|| FfiError::General {
        msg: format!("unknown authoring tier {key:?} (expected \"admin\" or \"self\")"),
    })
}

/// The shared feature-policy editor, held across the boundary: the draft lives
/// HERE, in shared Rust, and the app forwards keystrokes and paints
/// [`FfiPolicyEditor::view`]. Obtain via [`FfiFeaturesClient::open_editor`].
#[derive(uniffi::Object)]
pub struct FfiPolicyEditor {
    nest: Arc<NestClient>,
    editor: Mutex<PolicyEditor>,
}

// Helpers in a PLAIN impl: a `#[uniffi::export]` block exports its private
// methods too.
impl FfiPolicyEditor {
    fn snapshot(&self) -> PolicyEditor {
        self.editor.lock().expect("editor lock").clone()
    }

    /// Fold a write's result: re-seed from the fresh read on success.
    fn settle(
        &self,
        result: Result<fauna_client_features::Saved, SaveError<fauna_client::NestClientError>>,
    ) -> Result<FfiEditorVerdict, FfiError> {
        match result {
            Ok(saved) => {
                let mut editor = self.editor.lock().expect("editor lock");
                if let Some(next) = editor.reseeded(&saved.reply) {
                    *editor = next;
                }
                Ok(FfiEditorVerdict {
                    status: Some(saved.status),
                    invalid: None,
                })
            }
            Err(SaveError::Invalid(reason)) => Ok(FfiEditorVerdict {
                status: None,
                invalid: Some(reason),
            }),
            Err(SaveError::Rpc(e)) => Err(stringify(e)),
        }
    }
}

#[fauna_uniffi_async::export]
impl FfiPolicyEditor {
    /// Everything the editor paints, derived by the shared view-model.
    pub fn view(&self) -> FfiPolicyEditorView {
        self.editor.lock().expect("editor lock").view().into()
    }

    /// `feature-policy-editor-on-radio` (`true`) / `-off-radio` (`false`).
    pub fn set_on(&self, on: bool) {
        self.editor.lock().expect("editor lock").set_on(on);
    }

    /// `feature-policy-editor-cell-input[index]` — one keystroke's new text.
    pub fn set_cell(&self, index: u32, text: String) {
        self.editor
            .lock()
            .expect("editor lock")
            .set_cell(index as usize, text);
    }

    /// `feature-policy-editor-save-button` — parse, write the whole document,
    /// re-read.
    pub async fn save(&self) -> Result<FfiEditorVerdict, FfiError> {
        let editor = self.snapshot();
        let result = FeaturesClient::new(Arc::clone(&self.nest))
            .save(&editor)
            .await;
        self.settle(result)
    }

    /// `feature-policy-editor-remove-button` — send the absent policy, re-read.
    pub async fn remove(&self) -> Result<FfiEditorVerdict, FfiError> {
        let editor = self.snapshot();
        let result = FeaturesClient::new(Arc::clone(&self.nest))
            .remove(&editor)
            .await;
        self.settle(result)
    }
}

// ── The client object ──────────────────────────────────────────────────

/// Typed-call client for the `fauna.features.*` kinds over an authenticated
/// nest connection. Obtain via [`crate::FfiNestClient::features`].
#[derive(uniffi::Object)]
pub struct FfiFeaturesClient {
    nest: Arc<NestClient>,
}

impl FfiFeaturesClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> FeaturesClient<Arc<NestClient>> {
        FeaturesClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiFeaturesClient {
    /// `fauna.features.status` — every registry member's effective policy and
    /// remaining quota for the calling account, already folded into rows.
    ///
    /// `nest_capabilities` is the nest's advertised capability set
    /// (`NestInfoReply.capabilities`), which decides the `hidden` affordance: a
    /// member whose capability token the nest does not advertise is not carried
    /// by that build at all, and rendering its row would advertise a plane the
    /// artifact does not have. Pass an empty vec only if the set is genuinely
    /// unknown — every member that *has* a token then reads `hidden`.
    ///
    /// Rows come back complete, including members the caller is not limited on:
    /// "unrestricted" is an answer, and an absence could not be told apart from
    /// a nest that never heard of the feature.
    pub async fn status(
        &self,
        nest_capabilities: Vec<String>,
    ) -> Result<Vec<FfiFeatureRow>, FfiError> {
        let reply = self.client().status().await.map_err(stringify)?;
        Ok(feature_rows(&reply.features, &nest_capabilities)
            .into_iter()
            .map(FfiFeatureRow::from)
            .collect())
    }

    /// **The call an app makes** — the whole feature-limits surface, ready to
    /// render: [`FeaturesClient::rows`] joined with `fauna.nest.info`'s
    /// capability set and folded via `feature_rows`, exactly mirroring
    /// [`status`](Self::status) plus the capability read tui/linux/the wasm
    /// face already do through the shared crate directly. FFI needed its own
    /// mirror rather than exposing `nest_capabilities` as a separate call: no
    /// native FFI consumer had a way to obtain the capability set otherwise
    /// (`NestInfoReply.capabilities` had no UniFFI export at all — the same
    /// "no export, not just unconsumed" shape as the earlier trickle-down's
    /// `resolve_post`/`locate_card_by_uid_hash` gap).
    pub async fn rows(&self) -> Result<Vec<FfiFeatureRow>, FfiError> {
        Ok(self
            .client()
            .rows()
            .await
            .map_err(stringify)?
            .into_iter()
            .map(FfiFeatureRow::from)
            .collect())
    }
    /// One tier's authored documents as host rows (`tier`: `admin` | `self`) —
    /// `fauna.features.policy.get` / `fauna.features.self_limits.get` joined
    /// with the nest's capability set, unsupported members dropped.
    pub async fn authored_rows(&self, tier: String) -> Result<Vec<FfiAuthoredRow>, FfiError> {
        let tier = tier_of(&tier)?;
        Ok(self
            .client()
            .authored_surface(tier)
            .await
            .map_err(stringify)?
            .rows()
            .into_iter()
            .map(FfiAuthoredRow::from)
            .collect())
    }

    /// Open the shared editor over one member at `tier`, seeded from the
    /// tier's AUTHORED document (never the effective one — a save is a
    /// whole-document replace).
    pub async fn open_editor(
        &self,
        tier: String,
        feature: String,
    ) -> Result<Arc<FfiPolicyEditor>, FfiError> {
        let tier = tier_of(&tier)?;
        let surface = self
            .client()
            .authored_surface(tier)
            .await
            .map_err(stringify)?;
        let editor = surface
            .editor(tier, &feature)
            .ok_or_else(|| FfiError::General {
                msg: format!("this nest does not carry the feature {feature:?}"),
            })?;
        Ok(Arc::new(FfiPolicyEditor {
            nest: Arc::clone(&self.nest),
            editor: Mutex::new(editor),
        }))
    }

    /// The guardian host's rows for one ward (`family-policy-feature-limits-*`)
    /// — the ward's `fauna.family.status` entry mapped to the same rows the
    /// other two tiers read.
    ///
    /// `ward` is the ward's actor id, as every `FfiFamilyClient` call takes it.
    pub async fn ward_authored_rows(&self, ward: Vec<u8>) -> Result<Vec<FfiAuthoredRow>, FfiError> {
        let tier = guardian_tier(&ward)?;
        Ok(self
            .client()
            .authored_surface(tier)
            .await
            .map_err(stringify)?
            .rows()
            .into_iter()
            .map(FfiAuthoredRow::from)
            .collect())
    }

    /// Open the shared editor over one member at the guardian tier for `ward`.
    ///
    /// The ward-keyed twin of [`open_editor`](Self::open_editor): the guardian
    /// tier has no bare key, because a limit that names no ward binds nobody.
    pub async fn open_editor_for_ward(
        &self,
        ward: Vec<u8>,
        feature: String,
    ) -> Result<Arc<FfiPolicyEditor>, FfiError> {
        let tier = guardian_tier(&ward)?;
        let surface = self
            .client()
            .authored_surface(tier)
            .await
            .map_err(stringify)?;
        let editor = surface
            .editor(tier, &feature)
            .ok_or_else(|| FfiError::General {
                msg: format!("no editable limit for the feature {feature:?} on this ward"),
            })?;
        Ok(Arc::new(FfiPolicyEditor {
            nest: Arc::clone(&self.nest),
            editor: Mutex::new(editor),
        }))
    }
}

fn guardian_tier(ward: &[u8]) -> Result<AuthoringTier, FfiError> {
    let ward: [u8; 32] = ward.try_into().map_err(|_| FfiError::General {
        msg: format!("a ward actor id is 32 bytes, got {}", ward.len()),
    })?;
    Ok(AuthoringTier::Guardian { ward })
}

/// Every registry member's stable key, in registry order — for a surface that
/// must list the gated set before (or without) a nest read.
#[uniffi::export]
pub fn gated_feature_keys() -> Vec<String> {
    fauna_client_features::gated_feature_keys()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_features::test_fixtures::item;
    use fauna_client_features::{GatedFeature, RuleTier, feature_row};
    use fauna_core::feature_gate::{Availability, FeaturePolicy, entry};
    use fauna_protocol::discovery::capability;
    use fauna_protocol::features::FeatureStatusItem;

    fn mirror(it: &FeatureStatusItem, caps: &[String]) -> FfiFeatureRow {
        FfiFeatureRow::from(feature_row(it, caps))
    }

    /// The mirror must carry the attribution across the boundary — a row that
    /// arrived with numbers but no tier could not satisfy boundary 4, and the
    /// native UIs have no other source for it.
    #[test]
    fn the_mirror_preserves_the_bounds_and_their_tiers() {
        let feature = GatedFeature::P2pShare;
        let r = mirror(&item(feature, &[]), &[capability::P2P_SHARE.to_string()]);

        assert_eq!(r.feature, "p2p-share");
        assert_eq!(r.availability, "limit");
        assert_eq!(r.unit, "bytes");
        assert!(!r.cells.is_empty());
        for cell in &r.cells {
            assert_eq!(cell.tier, "structural");
            assert_eq!(cell.tier_label.key, "features.tier_structural");
            assert_eq!(cell.remaining, cell.limit - cell.observed);
        }
        let day = r
            .cells
            .iter()
            .find(|c| c.dimension == "operations" && c.window == "day")
            .expect("p2p-share bounds operations per day at tier 1");
        assert_eq!(day.limit, entry(feature).tier1.operations.per_day.unwrap());
    }

    #[test]
    fn a_denied_member_crosses_as_disabled_with_its_reason_and_tier() {
        let denied = FeaturePolicy {
            availability: Availability::Deny,
            ..Default::default()
        };
        let r = mirror(
            &item(GatedFeature::P2pShare, &[(RuleTier::Admin, denied)]),
            &[capability::P2P_SHARE.to_string()],
        );

        assert_eq!(r.affordance, "disabled");
        assert_eq!(r.denied_by.as_deref(), Some("admin"));
        assert_eq!(
            r.restriction.map(|t| t.key).as_deref(),
            Some("features.denied_by_admin")
        );
    }

    /// `registry()` is not cfg-gated, so a payments-excised nest still returns
    /// a payments row. It must cross as `hidden`, or the app advertises a plane
    /// the artifact does not carry.
    #[test]
    fn payments_crosses_as_hidden_without_the_subscriptions_token() {
        let it = item(GatedFeature::Payments, &[]);
        assert_eq!(mirror(&it, &[]).affordance, "hidden");
        assert_eq!(
            mirror(&it, &[capability::SUBSCRIPTIONS.to_string()]).affordance,
            "available"
        );
    }

    #[test]
    fn the_editor_view_mirror_carries_every_cell_and_the_remove_gate() {
        let item = fauna_protocol::features::AuthoredPolicyItem {
            feature: GatedFeature::P2pShare,
            policy: Some(FeaturePolicy::NO_OPINION),
            unreadable: false,
            ceiling: fauna_client_features::test_fixtures::item(GatedFeature::P2pShare, &[]).policy,
            extra: Default::default(),
        };
        let editor = PolicyEditor::open(AuthoringTier::Admin, item);
        let view = FfiPolicyEditorView::from(editor.view());
        assert_eq!(view.tier, "admin");
        assert_eq!(view.cells.len(), editor.cell_count());
        assert!(view.can_remove);
        assert!(tier_of("guardian").is_err());
    }

    #[test]
    fn the_gated_set_is_listable_without_a_nest_read() {
        assert_eq!(gated_feature_keys(), vec!["payments", "zaps", "p2p-share"]);
    }
}
