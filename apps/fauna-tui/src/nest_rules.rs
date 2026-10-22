//! The member's side of the nest rules on tui — the lead app (`nest-rules.md`
//! § The nest content policy → *Carriage*, *The nest source in the engine*;
//! § Acceptance at joining, and a change of rules).
//!
//! Everything that decides lives in `fauna_client_policy::NestRulesBook`; what
//! is left here is the tui-shaped glue:
//!
//! - where the book lives — `<config dir>/nest-rules/book.json`, install-scoped
//!   like the region record (the records are the nests' public signed
//!   documents, keyed by nest identity, never a secret), so a change made while
//!   the app was closed is still found by its newer sequence;
//! - the reads: the session nest's self-description at sign-in, and again on
//!   every `StaleSurfaces::nest_info` push (the nest's `fauna.nest.info_changed`
//!   on an accepted rules write) and reconnect — that re-read goes past the
//!   per-connection memo, since the memo would answer with the reply the change
//!   just made stale;
//! - the placeholder a nest verdict paints in place of an item, the one-line
//!   changed notice, and the Settings rules surface.
//!
//! tui is signed into one nest, so every item it reads is read through the
//! session nest: the book's rule sets for that nest are the engine's nest
//! source (`ContentPolicyState::set_nest_policies`).

use std::path::PathBuf;
use std::sync::Arc;

use fauna_client_policy::{NestRulesBook, NestRulesRead, SignedNestRules, StoredNestRules};
use fauna_core::content_category::ContentLabelEntry;
use fauna_core::obligation::{ComposedVerdict, RenderVerdict};
use fauna_i18n::strings::nest_rules as t;
use fauna_protocol::discovery::NestInfoReply;
use fauna_ui_ids as ids;

use crate::app::{App, DataMessage, UiMessage};
use crate::element::{Element, Gesture};

/// The attribute a nest placeholder carries naming its verb — what the
/// convention-17 invariant counts block placeholders by.
pub const VERDICT_ATTR: &str = "verdict";

/// The app's UI language — the admin's reason is shown in it where the admin
/// wrote one (`i18n/strings/en.yaml` is the one table today). The guardian's
/// reason is picked in it too (`crate::content_policy::guardian_reason`).
pub(crate) const UI_LANG: &str = "en";

fn book_path() -> Option<PathBuf> {
    crate::session::config_dir().map(|d| d.join("nest-rules").join("book.json"))
}

/// The book, the session nest it binds, and the changed notice.
#[derive(Default)]
pub struct NestRulesState {
    book: NestRulesBook,
    /// The session nest's identity, once its self-description was read —
    /// which nest every item on this device is read through.
    nest_id: Option<String>,
    nest: Option<Arc<fauna_client::NestClient>>,
    /// Whether this state persists (the production constructor) — unit tests
    /// never touch the real config dir.
    persist: bool,
    /// The one-line changed notice naming the nest, until the member opens the
    /// rules surface.
    pub changed_notice: Option<String>,
}

impl std::fmt::Debug for NestRulesState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NestRulesState")
            .field("nest_id", &self.nest_id)
            .field("changed_notice", &self.changed_notice)
            .finish_non_exhaustive()
    }
}

impl NestRulesState {
    /// Restore the book at app start, ahead of any session.
    pub fn launch() -> Self {
        let records: Vec<StoredNestRules> = book_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            book: NestRulesBook::restore(records),
            persist: true,
            ..Self::default()
        }
    }

    /// Bind a signed document as the session nest's, as a self-description
    /// read would — for render tests.
    #[cfg(test)]
    pub(crate) fn bind_for_test(
        &mut self,
        nest_id: &str,
        domain: &str,
        signed: SignedNestRules<'_>,
    ) -> NestRulesRead {
        self.nest_id = Some(nest_id.to_string());
        self.book.read(nest_id, domain, Some(signed))
    }

    /// Forget the session's nest on an identity change; the book is the
    /// device's and stays.
    pub fn clear_session(&mut self) {
        self.nest_id = None;
        self.nest = None;
        self.changed_notice = None;
    }

    fn persist(&self) {
        if !self.persist {
            return;
        }
        let Some(path) = book_path() else { return };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let bytes = serde_json::to_vec(&self.book.stored()).map_err(std::io::Error::other)?;
            std::fs::write(&path, bytes)
        };
        if let Err(e) = write() {
            tracing::warn!("nest rules: cannot persist the book: {e}");
        }
    }

    /// An item's labels with the factors the session nest's bundled scorers
    /// produce over it joined — its keyword rules over the plaintext, its
    /// `wasm` labelers over the whole scorer input — the input the composed
    /// verdict folds.
    pub fn join_labels<'a>(
        &self,
        content_id: Option<&[u8; 32]>,
        labels: &'a [ContentLabelEntry],
        input: impl FnOnce() -> fauna_core::scoring::LabelerPostInput,
    ) -> std::borrow::Cow<'a, [ContentLabelEntry]> {
        match &self.nest_id {
            Some(id) => self.book.join_labels(id, content_id, labels, input),
            None => std::borrow::Cow::Borrowed(labels),
        }
    }
}

/// Push the session nest's rule sets into the render engine — after every
/// accepted read and after an identity change reset the engine.
pub fn apply_rule_sets(app: &App) {
    let sets = app
        .nest_rules
        .nest_id
        .as_deref()
        .map(|id| app.nest_rules.book.rule_sets_for(id))
        .unwrap_or_default();
    app.content_policy.set_nest_policies(sets);
}

/// Attach the session's nest and read its self-description — at sign-in.
/// Reads through the connection's one memoised reply, which the landing load
/// shares.
pub fn attach_session(
    app: &mut App,
    nest: Arc<fauna_client::NestClient>,
    tx: &tokio::sync::mpsc::UnboundedSender<UiMessage>,
) {
    app.nest_rules.nest = Some(Arc::clone(&nest));
    let generation = app.session_generation;
    let tx = tx.clone();
    tokio::spawn(async move {
        match fauna_protocol::read_nest_info(nest.as_ref()).await {
            Ok(info) => {
                let _ = tx.send(UiMessage::Data(DataMessage::NestInfo(
                    generation,
                    Box::new(info),
                )));
            }
            Err(e) => tracing::warn!("nest rules: fauna.nest.info failed: {e}"),
        }
    });
}

/// Re-read the self-description past the memo — the `nest_info` push and the
/// reconnect sweep (`App::apply_resync`).
pub fn spawn_reread(app: &App) {
    let Some(nest) = app.nest_rules.nest.clone() else {
        return;
    };
    let generation = app.session_generation;
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let read: Result<NestInfoReply, _> = fauna_protocol::RpcRequester::request(
            nest.as_ref(),
            "fauna.nest.info",
            fauna_protocol::discovery::NestInfoRequest::default(),
        )
        .await;
        match read {
            Ok(info) => {
                let _ = tx.send(UiMessage::Data(DataMessage::NestInfo(
                    generation,
                    Box::new(info),
                )));
            }
            Err(e) => tracing::warn!("nest rules: fauna.nest.info re-read failed: {e}"),
        }
    });
}

/// Fold one self-description on the UI thread: verify and keep in shared Rust,
/// persist, re-arm the render engine, and on a newer sequence set the notice.
/// A refused read leaves the last-known rules in force.
pub fn apply_info(app: &mut App, generation: u64, info: &NestInfoReply) {
    if generation != app.session_generation {
        return;
    }
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
    let read = app
        .nest_rules
        .book
        .read(&info.nest_id, &info.domain, signed);
    app.nest_rules.nest_id = Some(info.nest_id.clone());
    match &read {
        NestRulesRead::Changed { domain, .. } => {
            app.nest_rules.changed_notice = Some(t::changed_notice(domain));
            app.nest_rules.persist();
        }
        NestRulesRead::First { .. } => app.nest_rules.persist(),
        NestRulesRead::NoRules | NestRulesRead::Unchanged => {}
        NestRulesRead::Refused(why) => {
            tracing::warn!("nest rules: self-description refused, last-known rules stay: {why:?}");
        }
    }
    apply_rule_sets(app);
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

/// The app's frame naming the nest, in the verb's words
/// (`fauna_core::obligation::nest_rule_notice`).
pub fn notice_text(p: &NestPlaceholder) -> String {
    fauna_core::obligation::nest_rule_notice(&p.domain, p.verdict)
        .resolve(fauna_i18n::strings::lookup)
}

/// The placeholder painted **in place of** a nest-withheld item: the frame
/// naming the nest, the nest's domain, and the admin's reason verbatim; a
/// `collapse` adds the reveal. `scope` is the item's own container
/// (`post-card[i]` on the feed), or `None` for a flat surface.
pub fn placeholder(
    p: &NestPlaceholder,
    reveal: Gesture,
    scope: Option<(&str, usize)>,
) -> Vec<Element> {
    let verb = if p.is_block() { "block" } else { "collapse" };
    let mut els = vec![
        Element::label(ids::NEST_BLOCKED_NOTICE, notice_text(p)).attr(VERDICT_ATTR, verb),
        Element::label(ids::NEST_BLOCKED_DOMAIN, p.domain.as_str()),
        Element::label(ids::NEST_BLOCKED_REASON, p.reason.as_str()),
    ];
    if !p.is_block() {
        els.push(Element::gesture_button(
            ids::NEST_COLLAPSED_REVEAL_BUTTON,
            t::REVEAL_BUTTON,
            true,
            reveal,
        ));
    }
    match scope {
        Some((container, i)) => els.into_iter().map(|e| e.within(container, i)).collect(),
        None => els,
    }
}

/// The convention-17 "a nest Block never renders silent" state field — the
/// region field's twin (`crate::region::block_render_json`): `blocked` counted
/// at the verdict by each surface's own walk, `placeholders` off the page as
/// painted.
pub fn block_render_json(app: &App) -> serde_json::Value {
    let blocked = match app.page {
        crate::pages::Page::Feed => crate::feed::nest_blocked_count(app),
        crate::pages::Page::Conversations => crate::conversations::nest_blocked_count(app),
        _ => 0,
    };
    let placeholders = app
        .page_elements()
        .iter()
        .filter(|e| {
            e.id == ids::NEST_BLOCKED_NOTICE
                && e.attrs
                    .iter()
                    .any(|(k, v)| k == VERDICT_ATTR && v == "block")
        })
        .count();
    serde_json::json!({ "blocked": blocked, "placeholders": placeholders })
}

/// The changed notice as a page's first line — the way to the rules surface
/// (Settings) rides on it.
pub fn changed_notice_elements(app: &App) -> Vec<Element> {
    app.nest_rules
        .changed_notice
        .as_ref()
        .map(|notice| {
            vec![Element::chrome_button(
                notice.clone(),
                Gesture::Nav(crate::pages::Page::Settings),
            )]
        })
        .unwrap_or_default()
}

/// The Settings rules surface (`settings-nest-rules-*`) — a paint of the
/// shared `NestRulesSummary`; absent while the session nest has published no
/// rules. Showing it is reading the change, so it clears the notice.
pub fn settings_elements(app: &App) -> Vec<Element> {
    let Some(accepted) = app
        .nest_rules
        .nest_id
        .as_deref()
        .and_then(|id| app.nest_rules.book.get(id))
    else {
        return Vec::new();
    };
    let summary = accepted.summary(UI_LANG);
    if summary.rules.is_empty() {
        return Vec::new();
    }
    let resolve =
        |text: &fauna_core::localized::LocalizedText| text.resolve(fauna_i18n::strings::lookup);
    let mut els = vec![
        Element::label(
            ids::SETTINGS_NEST_RULES_SECTION,
            t::settings_title(&summary.domain),
        ),
        Element::label(
            ids::SETTINGS_NEST_RULES_VERSION,
            t::version(
                &summary.sequence.to_string(),
                &fauna_core::format::format_unix_local(summary.issued_at),
            ),
        ),
    ];
    let item = ids::SETTINGS_NEST_RULES_ITEM;
    for (i, rule) in summary.rules.iter().enumerate() {
        let verdict = resolve(&rule.verdict);
        let line = match &rule.level {
            Some(level) => t::rule_line(&verdict, &resolve(level), &rule.reason),
            None => t::rule_line_no_level(&verdict, &rule.reason),
        };
        els.push(Element::label(item, line).within(item, i));
        if let Some(inert) = &rule.inert {
            els.push(
                Element::label(ids::SETTINGS_NEST_RULES_INERT_NOTICE, resolve(inert))
                    .within(item, i),
            );
        }
    }
    els
}
