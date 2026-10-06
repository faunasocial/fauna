//! The Settings → **Members To Review** sub-page (`ui.yaml` page
//! `member_review`; nav id `member-review`) — the **permanent** half of the
//! post-succession unattested-member review, and item (iv) of
//! `succession-aftermath.md` § Propagation's two-surface ruling.
//!
//! **What makes it the permanent one.** `succession-aftermath.md:87` — a
//! Settings rail sub-page directly after Account, holding whatever a review
//! sweep left unanswered. Unlike the (not-yet-built-on-linux) ephemeral
//! kit-side pass inside the Recovery Kit section, this
//! page has **no sweep gate**: it renders whatever is open on the succession ledger (`fauna.state.succession-ledger`)
//! whenever the user navigates to it, so a deferred backlog stays reachable
//! after the ceremony that raised it scrolls away. The two surfaces share one
//! row shape (`fauna_core::data::review_row_text` + the four `member-review-*`
//! ids) — tui's `settings/member_review.rs::review_rows` is the reference this
//! module ports; there is no ephemeral pass on linux yet to call it from.
//!
//! **The verdict is DERIVED, never chosen.** *Remove* drives
//! [`fauna_conversations::ConversationsManager::evict_person_everywhere`] over
//! the groups the person is in **now**, re-derived, and records only what
//! `CrossGroupEviction::earned_verdict()` earned. A partial eviction earns
//! none, so the row **stays** and the page's `error-message` says how far it
//! got — an app that wrote `Removed` from its own reasoning would silence a
//! row whose person is still seated in the groups that refused.
//!
//! **`member-review-empty` is not a safety verdict.** The page holds only a
//! backlog somebody explicitly postponed, so it is empty before any recovery
//! and empty again once the backlog is worked through
//! (`succession-aftermath.md` § Implementation status today leaves
//! deliberately no combined "is the user safe" boolean for a surface to round
//! a count of zero up to).
//!
//! Reads the roster on its **nav edge** (`settings_shell.rs`'s
//! `connect_visible_child_name_notify` "member-review" arm) — a deliberate,
//! narrow read-on-nav for a page that is opened rarely and is exactly where a
//! verdict another device recorded must not be re-asked.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;

use fauna_conversations::ConversationsManager;
use fauna_conversations::eviction::{CrossGroupEviction, UnreachableSeatClass};
use fauna_core::data::{MemberReview, UnattestedVerdict};
use fauna_core::identity::ActorId;

use crate::async_helper::{hydrate_with_retry, spawn_with_snapshot};
use crate::client::FaunaClient;
use crate::i18n::strings::settings::member_review_page as page;
use crate::i18n::strings::settings::recovery_kit as t;
use crate::testid::set_test_id;

/// One surface's repaint, run with the roster that just landed.
type Repaint = Box<dyn Fn(&[MemberReview])>;

/// This window's post-succession review roster — the ONE copy both linux
/// renderings read: the member-chip pair on the conversations page and the
/// contacts badge (`identity-succession.md` § Propagation → *MLS groups*,
/// item 3a). tui holds it as one `App` field, web as one module store, apple as
/// one `FaunaClient` cache; one copy is what keeps two surfaces from
/// disagreeing about who is flagged.
///
/// **One writer.** Only `app.rs`'s `DataMessage::MemberReviewsLoaded` arm calls
/// [`Roster::set`], so every refresh point lands through that message: the
/// aftermath's `config_stage_settled` hook, the post-auth and resync reads, the
/// contacts page's nav edge, and the re-read after each Keep or Remove on any
/// review surface. A failed read sends no roster, so the marks on screen stand
/// rather than blanking.
///
/// ⚠ There is deliberately no read on the conversations page's own nav edge:
/// a member list paints constantly for state that almost never moves
/// (`succession-propagation.md` § Implementation status today, the permanent
/// view's bullet).
#[derive(Default)]
pub struct Roster {
    reviews: RefCell<Vec<MemberReview>>,
    on_changed: RefCell<Vec<Repaint>>,
}

impl Roster {
    /// The roster as it stands.
    pub fn reviews(&self) -> std::cell::Ref<'_, Vec<MemberReview>> {
        self.reviews.borrow()
    }

    /// Replace the roster and repaint every surface that subscribed, now. The
    /// conversations page otherwise repaints only on its manager's snapshot
    /// ticks, so a roster landing between two of them would stay unseen until
    /// the open thread next changed.
    pub fn set(&self, reviews: Vec<MemberReview>) {
        *self.reviews.borrow_mut() = reviews;
        let reviews = self.reviews.borrow();
        for repaint in self.on_changed.borrow().iter() {
            repaint(&reviews);
        }
    }

    /// Call `repaint` with the new roster each time [`Roster::set`] lands one.
    pub fn connect_changed(&self, repaint: impl Fn(&[MemberReview]) + 'static) {
        self.on_changed.borrow_mut().push(Box::new(repaint));
    }
}

/// Widget handles the render + event closures need.
struct Widgets {
    error_label: gtk::Label,
    list: gtk::Box,
    placeholder: gtk::Label,
    rows: RefCell<Vec<gtk::Box>>,
}

/// Everything the handlers + render need.
struct Ctx {
    rt: tokio::runtime::Handle,
    manager: Arc<ConversationsManager>,
    /// Re-read the window's app-wide [`Roster`] once a verdict lands here, so
    /// the member-chip pair and the contacts badge follow this page's answer
    /// (`FaunaClient::fetch_member_reviews` in production).
    refresh_roster: Box<dyn Fn()>,
    w: Widgets,
}

/// Build the page's widget tree, with no client — the exact tree
/// [`build_member_review_page`] wires, split out so a unit test can assert
/// every static ui.yaml ID with no `FaunaClient` in play (mirrors
/// `settings::muted_words`'s `build_page_widgets`/`wire` split).
fn build_page_widgets() -> (gtk::Box, Widgets) {
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    let intro = gtk::Label::new(Some(page::INTRO));
    intro.set_halign(gtk::Align::Start);
    intro.set_wrap(true);
    intro.add_css_class("dim-label");
    intro.add_css_class("caption");
    outer.append(&intro);

    // error-message — every page has one (Rule 2), hidden until set. Doubles as
    // the surface for a partial-Remove's "how far it got" message — the row it
    // is about stays on screen, so the explanation belongs beside it rather
    // than replacing it.
    let error_label = gtk::Label::builder().visible(false).build();
    error_label.add_css_class("error");
    error_label.set_halign(gtk::Align::Start);
    error_label.set_wrap(true);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    outer.append(&error_label);

    // member-review-empty — the page's ordinary state (empty before any
    // recovery, empty again once the backlog is worked through), not a safety
    // verdict. Mutually exclusive with the row list. Hidden until the first
    // read resolves — "loading is not empty" (`docs/goal/ui/README.md`).
    let placeholder = gtk::Label::builder().visible(false).build();
    placeholder.set_label(page::EMPTY);
    placeholder.set_halign(gtk::Align::Start);
    placeholder.add_css_class("dim-label");
    set_test_id(&placeholder, ids::MEMBER_REVIEW_EMPTY);
    outer.append(&placeholder);

    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    outer.append(&list);

    let widgets = Widgets {
        error_label,
        list,
        placeholder,
        rows: RefCell::new(Vec::new()),
    };
    (outer, widgets)
}

/// Wire the widgets to the shared review seams, load on mount, and hand back
/// the shared `ctx` so the caller can rebuild a nav-edge refresh closure over
/// the same widgets — same split `settings::muted_words::wire` uses.
fn wire(client: &Rc<FaunaClient>, widgets: Widgets) -> Rc<Ctx> {
    let ctx = Rc::new(Ctx {
        rt: client.runtime_handle(),
        manager: crate::conversations::manager(),
        refresh_roster: {
            let client = Rc::clone(client);
            Box::new(move || client.fetch_member_reviews())
        },
        w: widgets,
    });
    refresh(&ctx);
    ctx
}

/// Build the "Members To Review" page. Returns `(page, refresh)` — the caller
/// (`settings_shell.rs`) wires `refresh` to the sub-stack's
/// `connect_visible_child_name_notify` "member-review" arm, the page's nav-edge
/// read.
pub fn build_member_review_page(client: &Rc<FaunaClient>) -> (gtk::Box, impl Fn() + 'static) {
    let (outer, widgets) = build_page_widgets();
    let ctx = wire(client, widgets);
    let page_box = crate::testid::wrap_page_with_heading(page::TITLE, ids::PAGE_HEADING, &outer);
    let refresh_fn = move || refresh(&ctx);
    (page_box, refresh_fn)
}

/// Read the open review roster from the succession ledger via the shared
/// `fauna_client_config::load_member_reviews` seam, over the account-store
/// handle.
pub(crate) async fn load_reviews() -> Result<Vec<MemberReview>, String> {
    let store = crate::account_runtime::ledger_store()?;
    fauna_client_config::load_member_reviews(&store)
        .await
        .map_err(|e| e.to_string())
}

/// Re-read the roster on mount and on every nav-edge visit.
fn refresh(ctx: &Rc<Ctx>) {
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move {
            // Kept: load_reviews is an account-store read, not a single
            // NestClient RPC the transport parks (transport.md § Request
            // lifecycle step 3's note).
            hydrate_with_retry(load_reviews).await
        },
        move |result| match result {
            Ok(reviews) => {
                ctx_render.w.error_label.set_visible(false);
                render_rows(&ctx_render, &reviews);
            }
            Err(e) => {
                // A failed read leaves the cached roster on screen alone rather
                // than blanking it — an empty page is indistinguishable from
                // "you are done", and this one must never say that by accident.
                super::render_error_label(&ctx_render.w.error_label, Some(&e));
            }
        },
    );
}

/// Record the owner's **Keep** verdict, then re-read the roster (the answered
/// row drops out).
pub(crate) async fn keep_person(person: ActorId) -> Result<(), String> {
    let store = crate::account_runtime::ledger_store()?;
    fauna_client_config::decide_member_review(&store, &person, UnattestedVerdict::Kept)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// One **Remove** round trip's outcome: the eviction the manager achieved, the
/// name resolved before evicting (there is no seat left to read a handle off
/// afterward), and whether persisting an earned verdict failed.
struct RemoveOutcome {
    eviction: CrossGroupEviction,
    who: String,
    decide_error: Option<String>,
}

/// Evict `person` from every group of the owner's they are in now, then persist
/// only what the eviction actually earned. Never writes `Removed` from its own
/// reasoning — [`CrossGroupEviction::earned_verdict`] is the only source of the
/// verdict.
async fn remove_person(
    manager: Arc<ConversationsManager>,
    person: ActorId,
) -> Result<RemoveOutcome, String> {
    // Resolve the name BEFORE evicting: `handle_for_person` reads live
    // membership, so after a successful eviction there is no seat left to read
    // a handle off.
    let who = manager
        .handle_for_person(&person)
        .unwrap_or_else(|| t::REVIEW_UNKNOWN_PERSON.to_string());
    let eviction = manager.evict_person_everywhere(&person).await;
    let decide_error = match eviction.earned_verdict() {
        Some(verdict) => {
            let store = crate::account_runtime::ledger_store()?;
            fauna_client_config::decide_member_review(&store, &person, verdict)
                .await
                .err()
                .map(|e| e.to_string())
        }
        None => None,
    };
    Ok(RemoveOutcome {
        eviction,
        who,
        decide_error,
    })
}

/// The `error-message` text a Remove outcome leaves behind — `None` when the
/// row is expected to have dropped out cleanly (a full re-read follows either
/// way, so a stale write between the eviction and the CAS still self-heals on
/// the next visit).
fn remove_result_message(outcome: &RemoveOutcome) -> Option<String> {
    if let Some(err) = &outcome.decide_error {
        return Some(t::review_verdict_failed(&outcome.who, err));
    }
    if outcome.eviction.earned_verdict().is_some() {
        return None;
    }
    let e = &outcome.eviction;
    let mut parts = Vec::new();
    if !e.failed.is_empty() {
        let groups = e.evicted.len() + e.failed.len();
        parts.push(t::review_remove_partial(
            &outcome.who,
            &e.evicted.len().to_string(),
            &groups.to_string(),
        ));
    } else if !e.evicted.is_empty() {
        parts.push(t::review_remove_done_here(
            &outcome.who,
            &e.evicted.len().to_string(),
        ));
    } else {
        parts.push(t::review_remove_none_here(&outcome.who));
    }
    let folders = e
        .unreachable
        .iter()
        .filter(|s| s.class == UnreachableSeatClass::FolderChannel)
        .count();
    if folders > 0 {
        parts.push(t::review_remove_folder_seats(&folders.to_string()));
    }
    let unsynced = e
        .unreachable
        .iter()
        .filter(|s| s.class == UnreachableSeatClass::ChatGroupNoThreadHere)
        .count();
    if unsynced > 0 {
        parts.push(t::review_remove_unsynced_seats(&unsynced.to_string()));
    }
    Some(parts.join(" "))
}

/// Rebuild the row list from the roster. Mutually exclusive with
/// `member-review-empty`.
fn render_rows(ctx: &Rc<Ctx>, reviews: &[MemberReview]) {
    let mut rows = ctx.w.rows.borrow_mut();
    for row in rows.drain(..) {
        ctx.w.list.remove(&row);
    }
    for (i, review) in reviews.iter().enumerate() {
        let row = build_review_row(ctx, review, i);
        ctx.w.list.append(&row);
        rows.push(row);
    }
    ctx.w.placeholder.set_visible(reviews.is_empty());
    ctx.w.list.set_visible(!reviews.is_empty());
}

/// One `member-review-row` for `review`, with `member-review-keep-button` /
/// `member-review-remove-button` scoped **inside** it (the e2e scope
/// convention — a driver acting on row `[i]` is always acting on the person
/// row `[i]` names).
fn build_review_row(ctx: &Rc<Ctx>, review: &MemberReview, _i: usize) -> gtk::Box {
    let handle = ctx.manager.handle_for_person(&review.person);
    let parts = fauna_core::data::review_row_text(review, handle.as_deref());
    let label_text = t::review_row(
        &parts.who.resolve(crate::i18n::strings::lookup),
        &fauna_core::data::reason_text(&parts.reasons, crate::i18n::strings::lookup),
    );

    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    row.update_property(&[
        gtk::accessible::Property::Description("member-review-row"),
        gtk::accessible::Property::Label(label_text.as_str()),
    ]);
    row.set_widget_name("member-review-row");

    let text_label = gtk::Label::new(Some(&label_text));
    text_label.set_halign(gtk::Align::Start);
    text_label.set_wrap(true);
    text_label.set_hexpand(true);
    row.append(&text_label);

    let keep = gtk::Button::with_label(t::REVIEW_KEEP);
    keep.set_valign(gtk::Align::Center);
    set_test_id(&keep, ids::MEMBER_REVIEW_KEEP_BUTTON);
    crate::offline_gate::declare_wire_kind(&keep, "fauna.account.state.put");
    {
        let ctx = Rc::clone(ctx);
        let person = review.person;
        keep.connect_clicked(move |_| dispatch_keep(&ctx, person));
    }
    row.append(&keep);

    let remove = gtk::Button::with_label(t::REVIEW_REMOVE);
    remove.set_valign(gtk::Align::Center);
    remove.add_css_class("destructive-action");
    set_test_id(&remove, ids::MEMBER_REVIEW_REMOVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&remove, "fauna.conversations.channel.send");
    {
        let ctx = Rc::clone(ctx);
        let person = review.person;
        remove.connect_clicked(move |_| dispatch_remove(&ctx, person));
    }
    row.append(&remove);

    row
}

fn dispatch_keep(ctx: &Rc<Ctx>, person: ActorId) {
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { keep_person(person).await },
        move |result| match result {
            Ok(()) => {
                refresh(&ctx_render);
                (ctx_render.refresh_roster)();
            }
            Err(e) => super::render_error_label(&ctx_render.w.error_label, Some(&e)),
        },
    );
}

fn dispatch_remove(ctx: &Rc<Ctx>, person: ActorId) {
    let manager = ctx.manager.clone();
    let ctx_render = Rc::clone(ctx);
    spawn_with_snapshot(
        &ctx.rt,
        move || async move { remove_person(manager, person).await },
        move |result| match result {
            Ok(outcome) => {
                super::render_error_label(
                    &ctx_render.w.error_label,
                    remove_result_message(&outcome).as_deref(),
                );
                // Re-read regardless: a full eviction drops the row, a partial
                // one is re-rendered from the same (unchanged) ledger state.
                refresh(&ctx_render);
                (ctx_render.refresh_roster)();
            }
            Err(e) => super::render_error_label(&ctx_render.w.error_label, Some(&e)),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::MemberUnattestedReason;

    fn person(b: u8) -> ActorId {
        ActorId([b; 32])
    }

    fn open_review(b: u8) -> MemberReview {
        MemberReview {
            person: person(b),
            reasons: vec![MemberUnattestedReason::CompromiseWindow],
        }
    }

    /// `remove_result_message` — a complete eviction (earns `Removed`) leaves
    /// nothing to say; the row is expected to drop out on the next re-read.
    #[test]
    fn a_complete_eviction_leaves_no_message() {
        let outcome = RemoveOutcome {
            eviction: CrossGroupEviction {
                evicted: vec![],
                failed: vec![],
                unreachable: vec![],
            },
            who: "someone".to_string(),
            decide_error: None,
        };
        assert_eq!(remove_result_message(&outcome), None);
    }

    /// A failed CAS write is reported even when the eviction itself succeeded —
    /// the row may still be visible next visit, and the user should know why.
    #[test]
    fn a_failed_verdict_write_is_reported() {
        let outcome = RemoveOutcome {
            eviction: CrossGroupEviction {
                evicted: vec![],
                failed: vec![],
                unreachable: vec![],
            },
            who: "someone".to_string(),
            decide_error: Some("network error".to_string()),
        };
        assert!(remove_result_message(&outcome).unwrap().contains("someone"));
    }

    /// `reason_text` joins in raise order without dropping anything.
    #[test]
    fn reason_text_joins_all_reasons() {
        let review = MemberReview {
            person: person(4),
            reasons: vec![
                MemberUnattestedReason::CompromiseWindow,
                MemberUnattestedReason::Other("second".into()),
            ],
        };
        let parts = fauna_core::data::review_row_text(&review, None);
        let joined = fauna_core::data::reason_text(&parts.reasons, crate::i18n::strings::lookup);
        assert!(joined.contains(t::REVIEW_REASON_COMPROMISE));
        assert!(joined.contains(t::REVIEW_REASON_OTHER));
    }

    #[test]
    fn open_review_has_one_reason() {
        let review = open_review(1);
        assert_eq!(review.reasons.len(), 1);
    }

    /// The page exposes every static ui.yaml ID with no registered client
    /// (`build_member_review_page` requires a real `FaunaClient` for `wire`,
    /// so this test exercises the client-free `build_page_widgets` split, the
    /// exact widget tree the real page builds before wiring).
    #[test]
    fn member_review_page_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let (page, _widgets) = build_page_widgets();
            let wrapped =
                crate::testid::wrap_page_with_heading(page::TITLE, ids::PAGE_HEADING, &page);
            let names = crate::testid::widget_names(&wrapped);
            for id in ["page-heading", "error-message", "member-review-empty"] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}"
                );
            }
        });
    }

    /// The empty state is not shown until a read resolves — "loading is not
    /// empty" (`docs/goal/ui/README.md`). The freshly-built page — exactly
    /// what a user sees between navigating and the first reply — must not
    /// claim the backlog is empty.
    #[test]
    fn the_empty_state_is_hidden_until_a_read_resolves() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let (_page, widgets) = build_page_widgets();
            assert!(
                !widgets.placeholder.is_visible(),
                "a page that has not read yet must not paint member-review-empty"
            );
            assert!(
                !widgets.list.is_visible(),
                "a page that has not read yet must not paint an empty row list either"
            );
        });
    }

    /// `render_rows` toggles the row list and the empty state as mutual
    /// exclusives, and scopes each row's Keep/Remove pair inside it.
    #[test]
    fn render_rows_paints_rows_and_scopes_the_verdict_buttons() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let (_page, widgets) = build_page_widgets();
            // `render_rows`/`build_review_row` touch only `ctx.w` and
            // `ctx.manager` — `rt`/`refresh_roster` exist
            // only to type-check `Ctx` here, since the click-dispatch paths
            // that actually use them are not exercised by this test.
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("build a throwaway runtime");
            let ctx = Rc::new(Ctx {
                rt: rt.handle().clone(),
                manager: crate::conversations::manager(),
                refresh_roster: Box::new(|| {}),
                w: widgets,
            });
            render_rows(&ctx, &[open_review(1), open_review(2)]);
            assert!(!ctx.w.placeholder.is_visible());
            assert!(ctx.w.list.is_visible());
            let names = crate::testid::widget_names(&ctx.w.list);
            assert_eq!(
                names.iter().filter(|n| *n == "member-review-row").count(),
                2
            );
            assert_eq!(
                names
                    .iter()
                    .filter(|n| *n == "member-review-keep-button")
                    .count(),
                2
            );
            assert_eq!(
                names
                    .iter()
                    .filter(|n| *n == "member-review-remove-button")
                    .count(),
                2
            );
        });
    }
}
