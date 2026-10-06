//! The region content plane on linux (`region-blocking.md` § The content plane
//! → *How an app obtains its region's policy*, *The blocked render and the
//! transparency surface*) — a paint of the shared `fauna_client_region` plane,
//! lifted from tui's shape (`apps/fauna-tui/src/region.rs`).
//!
//! Everything that decides lives in `fauna_client_region`; what is left here is
//! the linux-shaped glue:
//!
//! - [`declared_region`] — **the platform leaf**: linux's OS region setting is
//!   the POSIX locale's territory (`LC_ALL`, else `LANG`), parsed by the same
//!   shared helper tui reads. There is no in-app override.
//! - where the device record lives — `$XDG_CONFIG_HOME/fauna/region/…`
//!   (`fauna_client_region::store`), install-scoped, so it survives sign-out
//!   and identity switch.
//! - the `thread_local!` storage strategy, the same one `crate::content_policy`
//!   uses (GTK hands the render callbacks no context to thread state through);
//! - the placeholder widget and the Settings region section.

use std::cell::RefCell;
use std::path::PathBuf;

use adw::prelude::*;
use fauna_client_region::{
    DeclaredRegion, PolicyState, RegionPlaceholder, RegionPlane, RegionSource, RegionVerb,
    RegionView, declared_from_posix_locale, effective_registry,
};
use fauna_core::content_category::ContentLabelEntry;
use fauna_core::obligation::ComposedVerdict;
use fauna_core::region_authority::RegionCode;
use fauna_core::scoring::LabelerPostInput;
use fauna_protocol::region::RegionArtifactGetReply;
use fauna_ui_ids as ids;
use gtk::glib;

use crate::i18n::strings::region as r;

/// The attribute a region placeholder carries naming its verb — what the
/// convention-17 invariant counts block placeholders by (tui's `VERDICT_ATTR`).
pub const VERDICT_ATTR: &str = "verdict";

/// The app's UI language — the authority's reason is shown in it where the
/// authority wrote one (the app's own strings are English today).
const UI_LANG: &str = "en";

struct State {
    plane: RegionPlane,
    last_refresh: Option<u64>,
}

thread_local! {
    /// The device's plane. Built empty so a unit test never reads the real
    /// config dir; [`launch`] replaces it with the declared, loaded one.
    static STATE: RefCell<State> = RefCell::new(State {
        plane: RegionPlane::new(None, effective_registry()),
        last_refresh: None,
    });
}

/// **The platform leaf.** linux's declared region: the POSIX locale's
/// territory — or, in a test-capable build, the e2e override
/// (`fauna_client_region::source::E2E_DECLARED_ENV`).
pub fn declared_region() -> Option<DeclaredRegion> {
    let leaf = declared_from_posix_locale(
        std::env::var("LC_ALL").ok().as_deref(),
        std::env::var("LANG").ok().as_deref(),
    );
    fauna_client_region::source::with_e2e_override(leaf, RegionSource::SystemLocale)
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs_or_zero().max(0) as u64
}

fn record_path() -> Option<PathBuf> {
    crate::window_state::dirs_config()
        .map(|d| fauna_client_region::store::record_path(&d.join("fauna")))
}

/// Build the plane and restore the device record — at app start, **ahead of**
/// the first fetch (§ Fail posture) — and arm the render engine with it.
pub fn launch() {
    let mut plane = RegionPlane::new(declared_region(), effective_registry());
    let bytes = record_path().and_then(|p| fauna_client_region::store::read_record(&p));
    plane.load(bytes.as_deref(), now_secs());
    STATE.with(|s| {
        *s.borrow_mut() = State {
            plane,
            last_refresh: None,
        }
    });
    apply_rule_sets();
}

/// Push the plane's rule sets into the render engine — after launch, after an
/// identity change reset the engine, and after every accepted reply.
pub fn apply_rule_sets() {
    let sets = STATE.with(|s| s.borrow().plane.rule_sets());
    crate::content_policy::set_region_policies(sets);
}

/// The chain to ask the relay about, stamping the refresh clock — `None` when
/// nothing is due (`force` at login ignores the cadence) or nothing is
/// declared/enrolled (no channel to ask about).
pub fn chain_to_refresh(force: bool) -> Option<Vec<RegionCode>> {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let now = now_secs();
        if !force && !fauna_client_region::refresh_due(s.last_refresh, now) {
            return None;
        }
        let chain = s.plane.chain();
        if chain.is_empty() {
            return None;
        }
        s.last_refresh = Some(now);
        Some(chain)
    })
}

/// Fold the relay's answers on the GTK main thread: verify in shared Rust,
/// persist the device record, and re-arm the render engine. A failed ask
/// writes nothing. Returns whether anything changed.
pub fn apply_replies(replies: Vec<(RegionCode, Result<RegionArtifactGetReply, String>)>) -> bool {
    let bytes = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.plane
            .apply_replies(replies, now_secs())
            .then(|| s.plane.to_bytes())
    });
    let Some(bytes) = bytes else {
        return false;
    };
    if let Some(path) = record_path()
        && let Err(e) = fauna_client_region::store::write_record(&path, &bytes)
    {
        tracing::warn!("region: cannot persist the device record: {e}");
    }
    apply_rule_sets();
    true
}

/// Forget the refresh clock on an identity change, so the next login asks at
/// once. The plane itself is the device's and stays.
pub fn clear_session() {
    STATE.with(|s| s.borrow_mut().last_refresh = None);
}

/// The composed verdict for one item, the region scorers' factors joined —
/// the ONE call the feed, the conversations bubble and the state walk make.
pub fn verdict_for(
    content_id_hex: &str,
    labels: &[ContentLabelEntry],
    input: impl FnOnce() -> LabelerPostInput,
) -> ComposedVerdict {
    let joined = STATE.with(|s| {
        let s = s.borrow();
        let id = fauna_core::hex32::decode(content_id_hex).ok();
        s.plane.join_labels(id.as_ref(), labels, input).into_owned()
    });
    crate::content_policy::verdict_for(&joined)
}

/// The region placeholder a composed verdict paints, when the region drove it.
pub fn region_verdict(verdict: &ComposedVerdict) -> Option<RegionPlaceholder> {
    fauna_client_region::placeholder_for(verdict, UI_LANG)
}

/// A feed card's scorer input.
pub fn post_input(post: &fauna_feed::PostSummary) -> LabelerPostInput {
    let author = fauna_core::hex32::decode(&post.author)
        .map(fauna_core::identity::ActorId)
        .unwrap_or(fauna_core::identity::ActorId([0; 32]));
    fauna_client_region::scorer_input(author, &post.body, &post.tags, post.has_media)
}

/// A conversation bubble's scorer input, post-decrypt (the zero id stands in
/// for the author, as on tui).
pub fn message_input(text: &str) -> LabelerPostInput {
    fauna_client_region::scorer_input(fauna_core::identity::ActorId([0; 32]), text, &[], false)
}

/// The app's frame for a placeholder: the region and its authority, in the
/// verb's words.
pub fn notice_text(p: &RegionPlaceholder) -> String {
    match p.verb {
        RegionVerb::Block => r::blocked_notice(p.region.as_str(), &p.authority_name),
        RegionVerb::Collapse => r::collapsed_notice(p.region.as_str(), &p.authority_name),
    }
}

fn dim_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("dim-label");
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label
}

/// The placeholder painted **in place of** a region-withheld item: the app's
/// frame naming the region and its authority, the authority's name, and its
/// reason verbatim; a `collapse` adds the reveal, which runs `on_reveal`.
pub fn placeholder_box(p: &RegionPlaceholder, on_reveal: impl Fn() + 'static) -> gtk::Box {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let notice = dim_label(&notice_text(p));
    crate::testid::set_test_id(&notice, ids::REGION_BLOCKED_NOTICE);
    crate::testid::set_test_attr(&notice, VERDICT_ATTR, p.verb.as_str());
    vbox.append(&notice);

    let authority = dim_label(&p.authority_name);
    crate::testid::set_test_id(&authority, ids::REGION_BLOCKED_AUTHORITY);
    vbox.append(&authority);

    let reason = dim_label(&p.reason);
    crate::testid::set_test_id(&reason, ids::REGION_BLOCKED_REASON);
    vbox.append(&reason);

    if p.verb == RegionVerb::Collapse {
        let reveal = gtk::Button::with_label(r::REVEAL_BUTTON);
        reveal.add_css_class("flat");
        reveal.set_halign(gtk::Align::Start);
        crate::testid::set_test_id(&reveal, ids::REGION_COLLAPSED_REVEAL_BUTTON);
        reveal.connect_clicked(move |_| on_reveal());
        vbox.append(&reveal);
    }
    vbox
}

/// One surface's verdict-side walk, live while its root widget is mapped.
type BlockCounter = (glib::WeakRef<gtk::Widget>, Box<dyn Fn() -> usize>);

thread_local! {
    static BLOCK_COUNTERS: RefCell<Vec<BlockCounter>> = const { RefCell::new(Vec::new()) };
}

/// Register a surface's convention-17 verdict-side walk: `count` answers how
/// many of the items the surface renders the region BLOCKS, re-deriving each
/// verdict from the snapshot (never from the widgets). It counts only while
/// `root` is mapped — on screen — which is the scope the painted side counts in.
pub fn register_block_counter(root: &impl IsA<gtk::Widget>, count: impl Fn() -> usize + 'static) {
    let weak = root.upcast_ref::<gtk::Widget>().downgrade();
    BLOCK_COUNTERS.with(|c| c.borrow_mut().push((weak, Box::new(count))));
}

/// The convention-17 "a region Block never renders silent" state field
/// (`tests/e2e-unified/helpers/frame_invariants.py`) — tui's
/// `region::block_render_json`: `blocked` summed over the on-screen surfaces'
/// verdict walks, `placeholders` counted off the mapped widget tree under
/// `root` as painted. An arm that drops the placeholder (or the item) shows up
/// as `placeholders < blocked`.
pub fn block_render_json(root: &impl IsA<gtk::Widget>) -> serde_json::Value {
    let blocked: usize = BLOCK_COUNTERS.with(|c| {
        let mut counters = c.borrow_mut();
        counters.retain(|(w, _)| w.upgrade().is_some());
        counters
            .iter()
            .filter(|(w, _)| w.upgrade().is_some_and(|w| w.is_mapped()))
            .map(|(_, count)| count())
            .sum()
    });
    fn painted(w: &gtk::Widget, class: &str) -> usize {
        if !w.is_mapped() {
            return 0;
        }
        let own =
            usize::from(w.widget_name() == ids::REGION_BLOCKED_NOTICE && w.has_css_class(class));
        let mut n = own;
        let mut c = w.first_child();
        while let Some(child) = c {
            n += painted(&child, class);
            c = child.next_sibling();
        }
        n
    }
    let class = format!("test-attr-{VERDICT_ATTR}-{}", RegionVerb::Block.as_str());
    let placeholders = painted(root.upcast_ref(), &class);
    serde_json::json!({ "blocked": blocked, "placeholders": placeholders })
}

/// Whether the region BLOCKS an item — the verdict half of the walk.
pub fn is_region_blocked(verdict: &ComposedVerdict) -> bool {
    region_verdict(verdict).is_some_and(|p| p.verb == RegionVerb::Block)
}

/// What the Settings region section paints.
pub fn view() -> RegionView {
    STATE.with(|s| s.borrow().plane.view(now_secs()))
}

fn source_text(source: RegionSource) -> &'static str {
    match source {
        RegionSource::Storefront => r::SOURCE_STOREFRONT,
        RegionSource::SystemRegion => r::SOURCE_SYSTEM_REGION,
        RegionSource::SystemLocale => r::SOURCE_SYSTEM_LOCALE,
        RegionSource::BrowserLocale => r::SOURCE_BROWSER_LOCALE,
    }
}

/// Repaint the Settings region section (`settings-region-*`) into `rows` — a
/// paint of the shared `RegionPlane::view`, never an app-side fold. `rows` is
/// a plain box the caller owns outright (the feature-limits section's
/// clear-and-rebuild container, `views/status.rs`).
pub fn paint_settings(rows: &gtk::Box) {
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    let view = view();
    let line = |id: &str, text: &str| {
        let label = gtk::Label::new(Some(text));
        label.set_halign(gtk::Align::Start);
        label.set_wrap(true);
        label.set_xalign(0.0);
        crate::testid::set_test_id(&label, id);
        label
    };
    let Some(declared) = &view.declared else {
        rows.append(&line(ids::SETTINGS_REGION_DECLARED, r::NONE_DECLARED));
        return;
    };
    rows.append(&line(
        ids::SETTINGS_REGION_DECLARED,
        &r::declared(declared.code.as_str()),
    ));
    rows.append(&line(
        ids::SETTINGS_REGION_SOURCE,
        source_text(declared.source),
    ));
    if view.policies.is_empty() {
        let none = gtk::Label::new(Some(r::NO_POLICY));
        none.set_halign(gtk::Align::Start);
        rows.append(&none);
    }
    for policy in &view.policies {
        let item = gtk::Box::new(gtk::Orientation::Vertical, 4);
        item.set_margin_top(6);
        item.set_margin_bottom(6);
        crate::testid::set_test_id(&item, ids::SETTINGS_REGION_POLICY_ITEM);
        item.append(&line(
            ids::SETTINGS_REGION_POLICY_AUTHORITY,
            &r::policy_authority(policy.region.as_str(), &policy.authority_name),
        ));
        item.append(&line(
            ids::SETTINGS_REGION_POLICY_VERSION,
            &r::policy_version(
                &policy.sequence.to_string(),
                &fauna_core::format::format_unix_local(policy.issued_at as i64),
            ),
        ));
        let notice = match &policy.state {
            PolicyState::Applied => None,
            PolicyState::Inert { version } => Some(r::inert_notice(&version.to_string())),
            PolicyState::Malformed(_) => Some(r::MALFORMED_NOTICE.to_string()),
        };
        if let Some(notice) = notice {
            item.append(&line(ids::SETTINGS_REGION_INERT_NOTICE, &notice));
        }
        rows.append(&item);
    }
    if let Some(checked) = view.last_checked_at {
        rows.append(&line(
            ids::SETTINGS_REGION_LAST_CHECKED,
            &r::last_checked(&fauna_core::format::format_unix_local(checked as i64)),
        ));
    }
    if view.stale {
        rows.append(&line(ids::SETTINGS_REGION_STALE_WARNING, r::STALE_WARNING));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_region::fixtures;
    use fauna_core::region_policy::ContentVerdict;

    fn nsfw() -> Vec<ContentLabelEntry> {
        vec![ContentLabelEntry {
            category: "nsfw".into(),
            confidence_per_mille: 900,
        }]
    }

    #[test]
    fn a_held_policy_reaches_the_verdict_and_names_its_authority() {
        let reason = "Withheld under the Synthetic Act, section 7.";
        let doc = fixtures::document(
            vec![fixtures::rule("nsfw", ContentVerdict::Block, reason)],
            Vec::new(),
        );
        STATE.with(|s| {
            *s.borrow_mut() = State {
                plane: fixtures::plane_holding(&doc, now_secs()),
                last_refresh: None,
            }
        });
        apply_rule_sets();
        let composed = verdict_for(&"ab".repeat(32), &nsfw(), || message_input("x"));
        let p = region_verdict(&composed).expect("the region drove the block");
        assert_eq!(p.verb, RegionVerb::Block);
        assert_eq!(p.reason, reason);
        assert_eq!(
            notice_text(&p),
            r::blocked_notice(fixtures::region().as_str(), &p.authority_name)
        );

        // An identity change keeps the device's plane (and its binding).
        crate::content_policy::clear_for_identity_change();
        clear_session();
        apply_rule_sets();
        let again = verdict_for(&"ab".repeat(32), &nsfw(), || message_input("x"));
        assert!(region_verdict(&again).is_some());

        // Reset for the next test on this thread.
        STATE.with(|s| s.borrow_mut().plane = RegionPlane::new(None, effective_registry()));
        apply_rule_sets();
    }
}
