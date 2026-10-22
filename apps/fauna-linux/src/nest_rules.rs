//! The member's side of the nest rules on linux (`nest-rules.md` § The nest
//! content policy → *Carriage*, *The nest source in the engine*; § Acceptance
//! at joining, and a change of rules) — a paint of the shared
//! `fauna_client_policy::NestRulesBook`, lifted from tui's shape
//! (`apps/fauna-tui/src/nest_rules.rs`).
//!
//! Everything that decides lives in the book; what is left here is the
//! linux-shaped glue:
//!
//! - where the book lives — `$XDG_CONFIG_HOME/fauna/nest-rules/book.json`,
//!   install-scoped like the region record (the records are the nests' public
//!   signed documents, keyed by nest identity, never a secret), so a change
//!   made while the app was closed is still found by its newer sequence;
//! - the fold of a self-description read (`FaunaClient::fetch_nest_rules` —
//!   at sign-in through the connection's memo, and past the memo on every
//!   `StaleSurfaces::nest_info` push and reconnect);
//! - the `thread_local!` storage strategy `crate::region` and
//!   `crate::content_policy` use (GTK hands the render callbacks no context);
//! - the placeholder a nest verdict paints in place of an item, the one-line
//!   changed notice, and the Settings rules surface.
//!
//! linux is signed into one nest, so every item it reads is read through the
//! session nest: the book's rule sets for that nest are the engine's nest
//! source (`ContentPolicyState::set_nest_policies`).

use std::borrow::Cow;
use std::cell::RefCell;
use std::path::PathBuf;

use adw::prelude::*;
use fauna_client_policy::{NestRulesBook, NestRulesRead, SignedNestRules, StoredNestRules};
use fauna_core::content_category::ContentLabelEntry;
use fauna_core::obligation::{ComposedVerdict, RenderVerdict};
use fauna_core::scoring::LabelerPostInput;
use fauna_protocol::discovery::NestInfoReply;
use fauna_ui_ids as ids;
use gtk::glib;

use crate::i18n::strings::nest_rules as t;

/// The attribute a nest placeholder carries naming its verb — what the
/// convention-17 invariant counts block placeholders by (tui's `VERDICT_ATTR`).
pub const VERDICT_ATTR: &str = "verdict";

/// The app's UI language — the admin's reason is shown in it where the admin
/// wrote one (the app's own strings are English today).
pub(crate) const UI_LANG: &str = "en";

#[derive(Default)]
struct State {
    book: NestRulesBook,
    /// The session nest's identity, once its self-description was read —
    /// which nest every item on this device is read through.
    nest_id: Option<String>,
    /// Whether this state persists ([`launch`]) — unit tests never touch the
    /// real config dir.
    persist: bool,
    /// The one-line changed notice naming the nest, until the member opens the
    /// rules surface.
    changed_notice: Option<String>,
    /// The shell's notice button and the Settings rules rows, repainted when a
    /// read lands.
    notice: Option<glib::WeakRef<gtk::Button>>,
    settings_rows: Option<glib::WeakRef<gtk::Box>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn book_path() -> Option<PathBuf> {
    crate::window_state::dirs_config().map(|d| d.join("fauna").join("nest-rules").join("book.json"))
}

/// Restore the book at app start, ahead of any session.
pub fn launch() {
    let records: Vec<StoredNestRules> = book_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.book = NestRulesBook::restore(records);
        s.persist = true;
    });
}

/// Forget the session's nest on an identity change; the book is the device's
/// and stays.
pub fn clear_session() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.nest_id = None;
        s.changed_notice = None;
    });
    apply_rule_sets();
    paint_notice();
}

fn persist(s: &State) {
    if !s.persist {
        return;
    }
    let Some(path) = book_path() else { return };
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec(&s.book.stored()).map_err(std::io::Error::other)?;
        std::fs::write(&path, bytes)
    };
    if let Err(e) = write() {
        tracing::warn!("nest rules: cannot persist the book: {e}");
    }
}

/// Push the session nest's rule sets into the render engine — after every
/// accepted read and after an identity change reset the engine.
pub fn apply_rule_sets() {
    let sets = STATE.with(|s| {
        let s = s.borrow();
        s.nest_id
            .as_deref()
            .map(|id| s.book.rule_sets_for(id))
            .unwrap_or_default()
    });
    crate::content_policy::set_nest_policies(sets);
}

/// Fold one self-description on the GTK main thread: verify and keep in
/// shared Rust, persist, re-arm the render engine and repaint the social
/// surfaces, and on a newer sequence set the notice. A refused read leaves
/// the last-known rules in force.
pub fn apply_info(info: &NestInfoReply) {
    let signed = info
        .moderation
        .content_policy
        .as_ref()
        .map(|s| SignedNestRules {
            document: &s.document,
            sequence: s.sequence,
            issued_at: s.issued_at,
            signature: &s.signature,
        });
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let read = s.book.read(&info.nest_id, &info.domain, signed);
        s.nest_id = Some(info.nest_id.clone());
        match &read {
            NestRulesRead::Changed { domain, .. } => {
                s.changed_notice = Some(t::changed_notice(domain));
                persist(&s);
            }
            NestRulesRead::First { .. } => persist(&s),
            NestRulesRead::NoRules | NestRulesRead::Unchanged => {}
            NestRulesRead::Refused(why) => {
                tracing::warn!(
                    "nest rules: self-description refused, last-known rules stay: {why:?}"
                );
            }
        }
    });
    apply_rule_sets();
    // The feed and the open thread compose the new sets on their repaint.
    crate::report::request_repaint();
    paint_notice();
    if let Some(rows) = settings_rows() {
        paint_settings(&rows);
    }
}

/// An item's labels with the factors the session nest's bundled scorers
/// produce over it joined — its keyword rules over the plaintext, its `wasm`
/// labelers over the whole scorer input.
pub fn join_labels<'a>(
    content_id: Option<&[u8; 32]>,
    labels: &'a [ContentLabelEntry],
    input: impl FnOnce() -> LabelerPostInput,
) -> Cow<'a, [ContentLabelEntry]> {
    STATE.with(|s| {
        let s = s.borrow();
        match &s.nest_id {
            Some(id) => Cow::Owned(
                s.book
                    .join_labels(id, content_id, labels, input)
                    .into_owned(),
            ),
            None => Cow::Borrowed(labels),
        }
    })
}

/// The nest placeholder a composed verdict paints, when a nest rule drove it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestPlaceholder {
    pub verdict: RenderVerdict,
    pub domain: String,
    /// The admin's reason, verbatim, in the app's language where written.
    pub reason: String,
}

impl NestPlaceholder {
    pub fn is_block(&self) -> bool {
        self.verdict == RenderVerdict::Block
    }
}

/// The placeholder a composed verdict paints, when the nest drove it.
pub fn nest_verdict(verdict: &ComposedVerdict) -> Option<NestPlaceholder> {
    let source = verdict.nest()?;
    Some(NestPlaceholder {
        verdict: verdict.verdict,
        domain: source.domain.clone(),
        reason: fauna_core::content_policy::reason_text(&source.reason, UI_LANG).to_string(),
    })
}

/// Whether a nest rule BLOCKS an item — the verdict half of the
/// convention-17 walk.
pub fn is_nest_blocked(verdict: &ComposedVerdict) -> bool {
    nest_verdict(verdict).is_some_and(|p| p.is_block())
}

/// The app's frame naming the nest, in the verb's words
/// (`fauna_core::obligation::nest_rule_notice`).
pub fn notice_text(p: &NestPlaceholder) -> String {
    fauna_core::obligation::nest_rule_notice(&p.domain, p.verdict)
        .resolve(crate::i18n::strings::lookup)
}

fn dim_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("dim-label");
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label.set_xalign(0.0);
    label
}

/// The placeholder painted **in place of** a nest-withheld item: the frame
/// naming the nest, the nest's domain, and the admin's reason verbatim; a
/// `collapse` adds the reveal, which runs `on_reveal`. A `block` has none —
/// a member never loosens a nest's rule.
pub fn placeholder_box(p: &NestPlaceholder, on_reveal: impl Fn() + 'static) -> gtk::Box {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let notice = dim_label(&notice_text(p));
    crate::testid::set_test_id(&notice, ids::NEST_BLOCKED_NOTICE);
    crate::testid::set_test_attr(
        &notice,
        VERDICT_ATTR,
        if p.is_block() { "block" } else { "collapse" },
    );
    vbox.append(&notice);

    let domain = dim_label(&p.domain);
    crate::testid::set_test_id(&domain, ids::NEST_BLOCKED_DOMAIN);
    vbox.append(&domain);

    let reason = dim_label(&p.reason);
    crate::testid::set_test_id(&reason, ids::NEST_BLOCKED_REASON);
    vbox.append(&reason);

    if !p.is_block() {
        let reveal = gtk::Button::with_label(t::REVEAL_BUTTON);
        reveal.add_css_class("flat");
        reveal.set_halign(gtk::Align::Start);
        crate::testid::set_test_id(&reveal, ids::NEST_COLLAPSED_REVEAL_BUTTON);
        reveal.connect_clicked(move |_| on_reveal());
        vbox.append(&reveal);
    }
    vbox
}

/// An item a published rule withholds — a region's or the nest's — as the
/// social surfaces paint it in place of the item. The composed verdict names
/// one source, so at most one arm applies (`nest-rules.md` decision 3).
#[derive(Debug, Clone)]
pub enum Withheld {
    Region(fauna_client_region::RegionPlaceholder),
    Nest(NestPlaceholder),
}

impl Withheld {
    /// The placeholder a composed verdict paints, when a region or the nest
    /// drove it.
    pub fn from_verdict(verdict: &ComposedVerdict) -> Option<Self> {
        crate::region::region_verdict(verdict)
            .map(Self::Region)
            .or_else(|| nest_verdict(verdict).map(Self::Nest))
    }

    /// A `block` — no reveal; a `collapse` keeps the engine's reveal.
    pub fn is_block(&self) -> bool {
        match self {
            Self::Region(p) => p.verb == fauna_client_region::RegionVerb::Block,
            Self::Nest(p) => p.is_block(),
        }
    }

    /// The app's frame naming whose rule withholds the item.
    pub fn notice_text(&self) -> String {
        match self {
            Self::Region(p) => crate::region::notice_text(p),
            Self::Nest(p) => notice_text(p),
        }
    }

    /// The placeholder widget, painted in place of the item.
    pub fn placeholder_box(&self, on_reveal: impl Fn() + 'static) -> gtk::Box {
        match self {
            Self::Region(p) => crate::region::placeholder_box(p, on_reveal),
            Self::Nest(p) => placeholder_box(p, on_reveal),
        }
    }
}

/// The convention-17 "a nest Block never renders silent" state field — the
/// region field's twin (`crate::region::block_render_json`): `blocked` summed
/// over the on-screen surfaces' verdict walks (`crate::region`'s registry,
/// whose walks take the source's predicate), `placeholders` counted off the
/// mapped widget tree under `root` as painted.
pub fn block_render_json(root: &impl IsA<gtk::Widget>) -> serde_json::Value {
    crate::region::block_render_json_for(
        root,
        &is_nest_blocked,
        ids::NEST_BLOCKED_NOTICE,
        &format!("test-attr-{VERDICT_ATTR}-block"),
    )
}

// ── the changed notice ──────────────────────────────────────────────────────

/// The changed notice's text, `None` once read — the e2e state field
/// `nest_rules_changed_notice` (the notice adds no element id,
/// `nest-rules.md` § Element IDs).
pub fn changed_notice() -> Option<String> {
    STATE.with(|s| s.borrow().changed_notice.clone())
}

/// Build the shell's one-line changed notice — a flat banner button naming
/// the nest, its way to the rules surface (Settings → Status). Hidden until a
/// read finds a newer sequence.
pub fn build_notice() -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("info-banner");
    button.set_halign(gtk::Align::Fill);
    button.set_visible(false);
    button.connect_clicked(|b| {
        crate::app::navigate_to_settings_subpage(b, "status");
    });
    STATE.with(|s| s.borrow_mut().notice = Some(button.downgrade()));
    paint_notice();
    button
}

fn paint_notice() {
    let (button, text) = STATE.with(|s| {
        let s = s.borrow();
        (
            s.notice.as_ref().and_then(|w| w.upgrade()),
            s.changed_notice.clone(),
        )
    });
    let Some(button) = button else { return };
    match text {
        Some(text) => {
            button.set_label(&text);
            button.set_visible(true);
        }
        None => button.set_visible(false),
    }
}

// ── the Settings rules surface ──────────────────────────────────────────────

fn settings_rows() -> Option<gtk::Box> {
    STATE.with(|s| s.borrow().settings_rows.as_ref().and_then(|w| w.upgrade()))
}

/// Register the Settings rules surface's container (the Status page builds
/// it) and paint it. Showing it is reading the change, so its mapping clears
/// the notice.
pub fn bind_settings(rows: &gtk::Box) {
    STATE.with(|s| s.borrow_mut().settings_rows = Some(rows.downgrade()));
    rows.connect_map(|rows| {
        STATE.with(|s| s.borrow_mut().changed_notice = None);
        paint_notice();
        paint_settings(rows);
    });
    paint_settings(rows);
}

/// Repaint the Settings rules surface (`settings-nest-rules-*`) into `rows` —
/// a paint of the shared `NestRulesSummary`; empty while the session nest has
/// published no rules.
pub fn paint_settings(rows: &gtk::Box) {
    while let Some(child) = rows.first_child() {
        rows.remove(&child);
    }
    let summary = STATE.with(|s| {
        let s = s.borrow();
        s.nest_id
            .as_deref()
            .and_then(|id| s.book.get(id))
            .map(|accepted| accepted.summary(UI_LANG))
    });
    let Some(summary) = summary.filter(|s| !s.rules.is_empty()) else {
        return;
    };
    let resolve =
        |text: &fauna_core::localized::LocalizedText| text.resolve(crate::i18n::strings::lookup);
    let line = |id: &str, text: &str| {
        let label = gtk::Label::new(Some(text));
        label.set_halign(gtk::Align::Start);
        label.set_wrap(true);
        label.set_xalign(0.0);
        crate::testid::set_test_id(&label, id);
        label
    };
    let section = gtk::Box::new(gtk::Orientation::Vertical, 4);
    section.set_margin_top(12);
    crate::testid::set_test_id(&section, ids::SETTINGS_NEST_RULES_SECTION);
    let title = gtk::Label::new(Some(&t::settings_title(&summary.domain)));
    title.add_css_class("heading");
    title.set_halign(gtk::Align::Start);
    section.append(&title);
    section.append(&line(
        ids::SETTINGS_NEST_RULES_VERSION,
        &t::version(
            &summary.sequence.to_string(),
            &fauna_core::format::format_unix_local(summary.issued_at),
        ),
    ));
    for rule in &summary.rules {
        let verdict = resolve(&rule.verdict);
        let text = match &rule.level {
            Some(level) => t::rule_line(&verdict, &resolve(level), &rule.reason),
            None => t::rule_line_no_level(&verdict, &rule.reason),
        };
        let item = line(ids::SETTINGS_NEST_RULES_ITEM, &text);
        section.append(&item);
        if let Some(inert) = &rule.inert {
            section.append(&line(
                ids::SETTINGS_NEST_RULES_INERT_NOTICE,
                &resolve(inert),
            ));
        }
    }
    rows.append(&section);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_policy::{RuleEditor, RuleKind, RuleVerdict};
    use fauna_core::content_policy::{ContentPolicyDocument, Publisher};

    const DOMAIN: &str = "nest.test";

    /// A nest's self-description carrying `document` signed at `sequence`.
    fn nest_info(document: &ContentPolicyDocument, sequence: u64) -> NestInfoReply {
        let nest = fauna_core::identity::ActorKeypair::from_secret([9u8; 32]);
        let signed = fauna_protocol::content_policy::SignedContentPolicy::sign(
            document,
            sequence,
            1_700_000_000 + sequence as i64,
            nest.signing_key(),
        )
        .unwrap();
        NestInfoReply {
            domain: DOMAIN.to_string(),
            nest_id: nest.actor_id_hex(),
            moderation: fauna_protocol::discovery::ModerationInfo {
                content_policy: Some(signed),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// One keywords rule on `terms` with `verdict`, as the admin's editor
    /// writes it.
    fn keyword_rules(terms: &str, verdict: RuleVerdict) -> ContentPolicyDocument {
        let mut editor = RuleEditor::load(None, Publisher::Nest(DOMAIN.to_string()));
        let r = editor.add_rule(RuleKind::Keywords).unwrap();
        editor.set_keywords_inline(r, terms);
        editor.set_verdict(r, verdict);
        editor.set_reason(r, "default", "No gambling on this nest");
        assert!(editor.can_save());
        editor.document()
    }

    fn verdict(text: &str) -> ComposedVerdict {
        crate::region::verdict_for(&"a1".repeat(32), &[], || crate::region::message_input(text))
    }

    fn find(root: &gtk::Widget, id: &str) -> Vec<gtk::Widget> {
        let mut out = Vec::new();
        if root.widget_name() == id {
            out.push(root.clone());
        }
        let mut c = root.first_child();
        while let Some(child) = c {
            out.extend(find(&child, id));
            c = child.next_sibling();
        }
        out
    }

    fn text(w: &gtk::Widget) -> String {
        w.clone()
            .downcast::<gtk::Label>()
            .unwrap()
            .text()
            .to_string()
    }

    /// The session nest's keyword rule withholds the matching item under the
    /// placeholder naming the nest and the admin's reason, with no reveal on a
    /// block; a newer sequence sets the changed notice; the rules surface
    /// names the version in force.
    #[test]
    fn a_nest_keyword_rule_withholds_the_item_and_a_change_is_announced() {
        crate::testid::run_on_gtk_thread(body);
    }

    fn body() {
        STATE.with(|s| *s.borrow_mut() = State::default());
        apply_info(&nest_info(&keyword_rules("casino", RuleVerdict::Block), 1));
        assert_eq!(changed_notice(), None, "the first read is no change");

        assert!(nest_verdict(&verdict("an ordinary post")).is_none());
        let composed = verdict("zzcasino night tonight");
        assert!(is_nest_blocked(&composed));
        let withheld = Withheld::from_verdict(&composed).expect("the nest drove it");
        assert!(withheld.is_block());
        let placeholder = withheld.placeholder_box(|| {}).upcast::<gtk::Widget>();
        assert_eq!(
            text(&find(&placeholder, ids::NEST_BLOCKED_NOTICE)[0]),
            t::blocked_notice(DOMAIN)
        );
        assert_eq!(
            text(&find(&placeholder, ids::NEST_BLOCKED_DOMAIN)[0]),
            DOMAIN
        );
        assert_eq!(
            text(&find(&placeholder, ids::NEST_BLOCKED_REASON)[0]),
            "No gambling on this nest"
        );
        assert!(find(&placeholder, ids::NEST_COLLAPSED_REVEAL_BUTTON).is_empty());

        apply_info(&nest_info(
            &keyword_rules("casino, lottery", RuleVerdict::Collapse),
            2,
        ));
        assert_eq!(changed_notice(), Some(t::changed_notice(DOMAIN)));
        let collapsed = Withheld::from_verdict(&verdict("a lottery ticket")).unwrap();
        assert!(!collapsed.is_block());
        let placeholder = collapsed.placeholder_box(|| {}).upcast::<gtk::Widget>();
        assert_eq!(
            find(&placeholder, ids::NEST_COLLAPSED_REVEAL_BUTTON).len(),
            1
        );

        let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        paint_settings(&rows);
        let rows = rows.upcast::<gtk::Widget>();
        assert_eq!(find(&rows, ids::SETTINGS_NEST_RULES_SECTION).len(), 1);
        assert!(text(&find(&rows, ids::SETTINGS_NEST_RULES_VERSION)[0]).starts_with("Version 2,"));
        assert_eq!(find(&rows, ids::SETTINGS_NEST_RULES_ITEM).len(), 1);

        // An identity change forgets the session nest: nothing binds.
        crate::content_policy::clear_for_identity_change();
        clear_session();
        assert!(nest_verdict(&verdict("zzcasino")).is_none());
        assert_eq!(changed_notice(), None);
    }
}
