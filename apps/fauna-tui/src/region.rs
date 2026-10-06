//! The region content plane on tui — the lead app (`region-blocking.md` § The
//! content plane → *How an app obtains its region's policy*, *The blocked render
//! and the transparency surface*).
//!
//! Everything that decides lives in `fauna_client_region`; what is left here is
//! the tui-shaped glue:
//!
//! - [`declared_region`] — **the platform leaf**, the one function the design
//!   allows to diverge per shell: tui's OS region setting is the POSIX locale's
//!   territory (`LC_ALL`, else `LANG`), parsed by the shared helper linux reads
//!   too. There is no in-app override.
//! - where the device record lives — `<config dir>/region/content-policy.cbor`,
//!   install-scoped (a region is a fact about the device, not the account), so
//!   it survives sign-out and identity switch; its bytes are public signed
//!   documents plus their replay floors, never a secret.
//! - the relay refresh at login and on the shared cadence, off the UI thread;
//! - the placeholder a region verdict paints in place of an item, and the
//!   Settings region section.

use std::path::PathBuf;
use std::sync::Arc;

use fauna_client_region::{
    DeclaredRegion, PolicyState, RegionPlaceholder, RegionPlane, RegionSource, RegionVerb,
    declared_from_posix_locale, effective_registry,
};
use fauna_core::content_category::ContentLabelEntry;
use fauna_core::obligation::ComposedVerdict;
use fauna_core::region_authority::RegionCode;
use fauna_core::scoring::LabelerPostInput;
use fauna_i18n::strings::region as r;
use fauna_protocol::region::RegionArtifactGetReply;
use fauna_ui_ids as ids;

use crate::app::{App, DataMessage, UiMessage};
use crate::element::{Element, Gesture};

/// The attribute a region placeholder carries naming its verb — what the
/// convention-17 invariant counts block placeholders by.
pub const VERDICT_ATTR: &str = "verdict";

/// **The platform leaf.** tui's declared region: the POSIX locale's territory
/// — or, in a test-capable build, the e2e override every app reads
/// (`fauna_client_region::source::E2E_DECLARED_ENV`).
pub fn declared_region() -> Option<DeclaredRegion> {
    let leaf = declared_from_posix_locale(
        std::env::var("LC_ALL").ok().as_deref(),
        std::env::var("LANG").ok().as_deref(),
    );
    fauna_client_region::source::with_e2e_override(leaf, RegionSource::SystemLocale)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn snapshot_path() -> Option<PathBuf> {
    crate::session::config_dir().map(|d| fauna_client_region::store::record_path(&d))
}

/// The app's region plane plus the nest it refreshes through.
pub struct RegionState {
    pub plane: RegionPlane,
    nest: Option<Arc<fauna_client::NestClient>>,
    last_refresh: Option<u64>,
}

impl RegionState {
    /// Build the plane and restore the device record — at app start, **ahead
    /// of** the first fetch (§ Fail posture).
    pub fn launch() -> Self {
        let mut plane = RegionPlane::new(declared_region(), effective_registry());
        let bytes = snapshot_path().and_then(|p| fauna_client_region::store::read_record(&p));
        plane.load(bytes.as_deref(), now_secs());
        Self {
            plane,
            nest: None,
            last_refresh: None,
        }
    }

    /// A plane with nothing declared and nothing loaded — the seam
    /// constructor's (unit tests never read the real config dir).
    pub fn empty() -> Self {
        Self {
            plane: RegionPlane::new(None, effective_registry()),
            nest: None,
            last_refresh: None,
        }
    }

    /// Forget the nest on an identity change; the plane itself is the
    /// device's and stays.
    pub fn clear_session(&mut self) {
        self.nest = None;
        self.last_refresh = None;
    }
}

impl std::fmt::Debug for RegionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegionState")
            .field("declared", &self.plane.declared())
            .finish_non_exhaustive()
    }
}

/// Push the plane's rule sets into the render engine — after launch, after an
/// identity change reset the engine, and after every accepted reply.
pub fn apply_rule_sets(app: &App) {
    app.content_policy
        .set_region_policies(app.region.plane.rule_sets());
}

/// Attach the session's nest and fire the first refresh — at login.
pub fn attach_session(
    app: &mut App,
    nest: Arc<fauna_client::NestClient>,
    tx: &tokio::sync::mpsc::UnboundedSender<UiMessage>,
) {
    app.region.nest = Some(nest);
    spawn_refresh(app, tx);
}

/// The minute tick's leg: refresh when the shared cadence is due.
pub fn refresh_if_due(app: &mut App, tx: &tokio::sync::mpsc::UnboundedSender<UiMessage>) {
    if fauna_client_region::refresh_due(app.region.last_refresh, now_secs()) {
        spawn_refresh(app, tx);
    }
}

fn spawn_refresh(app: &mut App, tx: &tokio::sync::mpsc::UnboundedSender<UiMessage>) {
    let Some(nest) = app.region.nest.clone() else {
        return;
    };
    let chain = app.region.plane.chain();
    if chain.is_empty() {
        // Nothing declared, or nothing enrolled for it: there is no channel to
        // ask about (and no false staleness alarm about one that does not exist).
        return;
    }
    app.region.last_refresh = Some(now_secs());
    let tx = tx.clone();
    tokio::spawn(async move {
        let replies = fauna_client_region::fetch_chain(nest.as_ref(), &chain).await;
        let _ = tx.send(UiMessage::Data(DataMessage::RegionReplies(replies)));
    });
}

/// Fold the relay's answers on the UI thread: verify in shared Rust, persist
/// the device record, and re-arm the render engine. A failed ask writes nothing.
pub fn apply_replies(
    app: &mut App,
    replies: Vec<(RegionCode, Result<RegionArtifactGetReply, String>)>,
) {
    if app.region.plane.apply_replies(replies, now_secs()) {
        persist(&app.region.plane);
        apply_rule_sets(app);
    }
}

fn persist(plane: &RegionPlane) {
    let Some(path) = snapshot_path() else { return };
    if let Err(e) = fauna_client_region::store::write_record(&path, &plane.to_bytes()) {
        tracing::warn!("region: cannot persist the device record: {e}");
    }
}

/// An item's labels with the region scorers' factors joined — the input the
/// composed verdict folds. Borrows the item's own labels when no scorer is in
/// force (every device today).
pub fn labels_for<'a>(
    app: &App,
    content_id_hex: &str,
    labels: &'a [ContentLabelEntry],
    input: impl FnOnce() -> LabelerPostInput,
) -> std::borrow::Cow<'a, [ContentLabelEntry]> {
    let id = fauna_core::hex32::decode(content_id_hex).ok();
    app.region.plane.join_labels(id.as_ref(), labels, input)
}

/// A feed card's scorer input — the same fields
/// `LabelerPostInput::from_post` maps, read off the card the app holds.
pub fn post_input(post: &fauna_feed::PostSummary) -> LabelerPostInput {
    let author = fauna_core::hex32::decode(&post.author)
        .map(fauna_core::identity::ActorId)
        .unwrap_or(fauna_core::identity::ActorId([0; 32]));
    fauna_client_region::scorer_input(author, &post.body, &post.tags, post.has_media)
}

/// The composed verdict for one item, the region scorers' factors joined — the
/// ONE call both surfaces and the invariant walk make, so the three cannot
/// disagree about what the engine said.
///
/// `reported` is what the viewer's own reports key this item by — the id a
/// report of it names (a post's cid, a message's record digest) and its
/// author — so an item the viewer reported renders the "you reported this"
/// placeholder (`moderation.md` § Corollary). `None` for a caller that only
/// asks what a *region* says.
pub fn verdict_for(
    app: &App,
    content_id_hex: &str,
    labels: &[ContentLabelEntry],
    reported: Option<ReportKey<'_>>,
    input: impl FnOnce() -> LabelerPostInput,
) -> ComposedVerdict {
    let joined = labels_for(app, content_id_hex, labels, input);
    match reported {
        Some(key) => app
            .content_policy
            .verdict_for_item(key.item_id, key.author_id, &joined),
        None => app.content_policy.verdict_for(&joined),
    }
}

/// How the viewer's own reports name one item — see [`verdict_for`].
#[derive(Debug, Clone, Copy)]
pub struct ReportKey<'a> {
    pub item_id: &'a str,
    pub author_id: Option<&'a str>,
}

/// A conversation bubble's scorer input, post-decrypt. A bubble carries no
/// author actor id the module could read, so the zero id stands in.
pub fn message_input(text: &str) -> LabelerPostInput {
    fauna_client_region::scorer_input(fauna_core::identity::ActorId([0; 32]), text, &[], false)
}

/// The app's UI language — the authority's reason is shown in it where the
/// authority wrote one. The app's own strings are English today
/// (`i18n/strings/en.yaml` is the one table).
const UI_LANG: &str = "en";

/// The region placeholder a composed verdict paints, when the region drove it
/// — the shared fold (`fauna_client_region::placeholder_for`).
pub fn region_verdict(verdict: &ComposedVerdict) -> Option<RegionPlaceholder> {
    fauna_client_region::placeholder_for(verdict, UI_LANG)
}

/// The placeholder painted **in place of** a region-withheld item: the app's
/// frame naming the region and its authority, the authority's name, and its
/// reason verbatim; a `collapse` adds the reveal. `scope` is the item's own
/// container (`post-card[i]` on the feed), or `None` for a flat surface.
pub fn placeholder(
    p: &RegionPlaceholder,
    reveal: Gesture,
    scope: Option<(&str, usize)>,
) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::REGION_BLOCKED_NOTICE, notice_text(p))
            .attr(VERDICT_ATTR, p.verb.as_str()),
        Element::label(ids::REGION_BLOCKED_AUTHORITY, p.authority_name.as_str()),
        Element::label(ids::REGION_BLOCKED_REASON, p.reason.as_str()),
    ];
    if p.verb == RegionVerb::Collapse {
        els.push(Element::gesture_button(
            ids::REGION_COLLAPSED_REVEAL_BUTTON,
            r::REVEAL_BUTTON,
            true,
            reveal,
        ));
    }
    match scope {
        Some((container, i)) => els.into_iter().map(|e| e.within(container, i)).collect(),
        None => els,
    }
}

/// The convention-17 "a region Block never renders silent" state field
/// (`tests/e2e-unified/helpers/frame_invariants.py`): `blocked` counted at the
/// verdict by each surface's own walk, `placeholders` counted off the page as
/// painted — two sides of the render, so an arm that drops the placeholder
/// (or the whole item) shows up as `placeholders < blocked`.
pub fn block_render_json(app: &App) -> serde_json::Value {
    let blocked = match app.page {
        crate::pages::Page::Feed => crate::feed::region_blocked_count(app),
        crate::pages::Page::Conversations => crate::conversations::region_blocked_count(app),
        _ => 0,
    };
    let placeholders = app
        .page_elements()
        .iter()
        .filter(|e| {
            e.id == ids::REGION_BLOCKED_NOTICE
                && e.attrs
                    .iter()
                    .any(|(k, v)| k == VERDICT_ATTR && v == "block")
        })
        .count();
    serde_json::json!({ "blocked": blocked, "placeholders": placeholders })
}

/// The Settings region section (`settings-region-*`) — a paint of the shared
/// `RegionPlane::view`, never an app-side fold.
pub fn settings_elements(app: &App) -> Vec<Element> {
    let view = app.region.plane.view(now_secs());
    let mut els = vec![Element::label(
        ids::SETTINGS_REGION_SECTION,
        r::SECTION_TITLE,
    )];
    let Some(declared) = &view.declared else {
        els.push(Element::label(
            ids::SETTINGS_REGION_DECLARED,
            r::NONE_DECLARED,
        ));
        return els;
    };
    els.push(Element::label(
        ids::SETTINGS_REGION_DECLARED,
        r::declared(declared.code.as_str()),
    ));
    els.push(Element::label(
        ids::SETTINGS_REGION_SOURCE,
        source_text(declared.source),
    ));
    if view.policies.is_empty() {
        els.push(Element::chrome(r::NO_POLICY));
    }
    for (i, policy) in view.policies.iter().enumerate() {
        let item = ids::SETTINGS_REGION_POLICY_ITEM;
        els.push(Element::label(item, " ").within(item, i));
        els.push(
            Element::label(
                ids::SETTINGS_REGION_POLICY_AUTHORITY,
                r::policy_authority(policy.region.as_str(), &policy.authority_name),
            )
            .within(item, i),
        );
        els.push(
            Element::label(
                ids::SETTINGS_REGION_POLICY_VERSION,
                r::policy_version(
                    &policy.sequence.to_string(),
                    &fauna_core::format::format_unix_local(policy.issued_at as i64),
                ),
            )
            .within(item, i),
        );
        let notice = match &policy.state {
            PolicyState::Applied => None,
            PolicyState::Inert { version } => Some(r::inert_notice(&version.to_string())),
            PolicyState::Malformed(_) => Some(r::MALFORMED_NOTICE.to_string()),
        };
        if let Some(notice) = notice {
            els.push(Element::label(ids::SETTINGS_REGION_INERT_NOTICE, notice).within(item, i));
        }
    }
    if let Some(checked) = view.last_checked_at {
        els.push(Element::label(
            ids::SETTINGS_REGION_LAST_CHECKED,
            r::last_checked(&fauna_core::format::format_unix_local(checked as i64)),
        ));
    }
    if view.stale {
        els.push(Element::label(
            ids::SETTINGS_REGION_STALE_WARNING,
            r::STALE_WARNING,
        ));
    }
    els
}

/// The app's frame for a placeholder: the region and its authority, in the
/// verb's words.
pub fn notice_text(p: &RegionPlaceholder) -> String {
    match p.verb {
        RegionVerb::Block => r::blocked_notice(p.region.as_str(), &p.authority_name),
        RegionVerb::Collapse => r::collapsed_notice(p.region.as_str(), &p.authority_name),
    }
}

fn source_text(source: RegionSource) -> &'static str {
    match source {
        RegionSource::Storefront => r::SOURCE_STOREFRONT,
        RegionSource::SystemRegion => r::SOURCE_SYSTEM_REGION,
        RegionSource::SystemLocale => r::SOURCE_SYSTEM_LOCALE,
        RegionSource::BrowserLocale => r::SOURCE_BROWSER_LOCALE,
    }
}
