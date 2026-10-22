//! The room's search strip (`room-search.md` § The surface): a field and its
//! submit under the thread header, then — once the home nest answered — one
//! row per hit in the served order, or the empty state.
//!
//! The strip is a pure paint of `ThreadDetail.room.search`: the shared
//! manager owns the query, the status and the rows (view state, never a
//! draft), and the field's text is local to this widget — gone with the strip.
//! In flight and failed paint no result region at all: a driver waits on a
//! row, the empty state or `error-message`, never on a "searching" element.
//! Every word on a row is the snapshot's; the `kind` attribute is for drivers
//! only. Mirrors tui's `room_search_elements`.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use fauna_conversations::room::{RoomSearchNav, RoomSearchSnapshot, RoomSearchStatus};

type SubmitCb = Rc<RefCell<Option<Box<dyn Fn(String)>>>>;
type OpenHitCb = Rc<RefCell<Option<Box<dyn Fn(RoomSearchNav)>>>>;

#[derive(Clone)]
pub struct RoomSearchStrip {
    /// The strip: hidden while the room's search is closed.
    pub root: gtk::Box,
    field: gtk::Entry,
    results: gtk::Box,
    /// What the result region was last built from, so a bare compose-body tick
    /// repaints no row (the rows are widgets a driver may be holding).
    painted: Rc<RefCell<Option<RoomSearchSnapshot>>>,
    on_submit: SubmitCb,
    on_open_hit: OpenHitCb,
}

impl RoomSearchStrip {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.set_margin_start(12);
        root.set_margin_end(12);
        root.set_margin_bottom(8);
        root.set_visible(false);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let field = gtk::Entry::new();
        field.set_hexpand(true);
        field.set_placeholder_text(Some(
            crate::i18n::strings::conversations::unified::ROOM_SEARCH_PLACEHOLDER,
        ));
        crate::testid::set_test_id(&field, ids::ROOM_SEARCH_FIELD);
        row.append(&field);
        let submit = gtk::Button::with_label(crate::i18n::strings::common::SEARCH);
        submit.add_css_class("flat");
        crate::testid::set_test_id(&submit, ids::ROOM_SEARCH_SUBMIT_BUTTON);
        // The query is a `room.search` call on the home nest; the kind is
        // constant for every submit, so the ENTRY declares it.
        crate::offline_gate::declare_wire_kind(&submit, "fauna.conversations.room.search");
        row.append(&submit);
        root.append(&row);

        let results = gtk::Box::new(gtk::Orientation::Vertical, 2);
        root.append(&results);

        let on_submit: SubmitCb = Rc::new(RefCell::new(None));
        let fire = {
            let cb = on_submit.clone();
            let field = field.clone();
            move || {
                if let Some(f) = cb.borrow().as_ref() {
                    f(field.text().to_string());
                }
            }
        };
        {
            let fire = fire.clone();
            field.connect_activate(move |_| fire());
        }
        submit.connect_clicked(move |_| fire());

        Self {
            root,
            field,
            results,
            painted: Rc::new(RefCell::new(None)),
            on_submit,
            on_open_hit: Rc::new(RefCell::new(None)),
        }
    }

    /// The submit (Enter in the field, or the button) hands the typed query up;
    /// a blank one is the manager's to drop.
    pub fn on_submit(&self, f: impl Fn(String) + 'static) {
        *self.on_submit.borrow_mut() = Some(Box::new(f));
    }

    /// A row's activation hands up its typed target.
    pub fn on_open_hit(&self, f: impl Fn(RoomSearchNav) + 'static) {
        *self.on_open_hit.borrow_mut() = Some(Box::new(f));
    }

    /// Paint `search` — `None` while the strip is closed on this thread.
    pub fn render(&self, search: Option<&RoomSearchSnapshot>) {
        let Some(search) = search else {
            if self.root.is_visible() {
                self.root.set_visible(false);
                // The draft is local and goes with the strip.
                self.field.set_text("");
                self.clear_results();
                *self.painted.borrow_mut() = None;
            }
            return;
        };
        if !self.root.is_visible() {
            self.root.set_visible(true);
            self.field.grab_focus();
        }
        if self.painted.borrow().as_ref() == Some(search) {
            return;
        }
        self.clear_results();
        if search.status == RoomSearchStatus::Done {
            if search.results.is_empty() {
                let empty = gtk::Label::new(Some(
                    crate::i18n::strings::conversations::unified::ROOM_SEARCH_NO_RESULTS,
                ));
                empty.set_halign(gtk::Align::Start);
                empty.add_css_class("dim-label");
                crate::testid::set_test_id(&empty, ids::ROOM_SEARCH_NO_RESULTS);
                self.results.append(&empty);
            }
            let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
            for hit in &search.results {
                let words = [
                    hit.sender_display.clone(),
                    hit.sent_at_ms
                        .map(|then| crate::i18n::conversation_timestamp(then, now_ms)),
                    Some(hit.text.clone()).filter(|t| !t.is_empty()),
                ];
                let text = words
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" \u{b7} ");
                let item = gtk::Button::with_label(&text);
                item.add_css_class("flat");
                if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                    label.set_halign(gtk::Align::Start);
                    label.set_xalign(0.0);
                    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                }
                crate::testid::set_test_id(&item, ids::ROOM_SEARCH_RESULT_ITEM);
                crate::testid::set_test_attr(&item, "kind", hit.kind.attr_token());
                let nav = hit.navigation.clone();
                let cb = self.on_open_hit.clone();
                item.connect_clicked(move |_| {
                    if let Some(f) = cb.borrow().as_ref() {
                        f(nav.clone());
                    }
                });
                self.results.append(&item);
            }
        }
        *self.painted.borrow_mut() = Some(search.clone());
    }

    fn clear_results(&self) {
        while let Some(child) = self.results.first_child() {
            self.results.remove(&child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_conversations::room::{RoomSearchResultKind, RoomSearchResultSnapshot};

    fn hit(kind: RoomSearchResultKind, nav: RoomSearchNav) -> RoomSearchResultSnapshot {
        RoomSearchResultSnapshot {
            kind,
            sender_display: Some("Ada".into()),
            sent_at_ms: None,
            text: "the words".into(),
            pending: false,
            navigation: nav,
        }
    }

    fn snapshot(
        status: RoomSearchStatus,
        results: Vec<RoomSearchResultSnapshot>,
    ) -> RoomSearchSnapshot {
        RoomSearchSnapshot {
            query: "words".into(),
            status,
            results,
        }
    }

    fn rows(strip: &RoomSearchStrip) -> Vec<gtk::Widget> {
        let mut out = Vec::new();
        let mut child = strip.results.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            out.push(widget);
        }
        out
    }

    /// Only a `Done` answer paints a result region: in flight and failed paint
    /// nothing a driver could mistake for an answer; `Done` with no rows is the
    /// empty state; rows keep the served order, each handing back its target.
    #[test]
    fn the_result_region_paints_only_a_done_answer() {
        crate::testid::run_on_gtk_thread(|| {
            let strip = RoomSearchStrip::new();
            let opened: Rc<RefCell<Vec<RoomSearchNav>>> = Rc::default();
            {
                let opened = opened.clone();
                strip.on_open_hit(move |nav| opened.borrow_mut().push(nav));
            }

            strip.render(None);
            assert!(!strip.root.is_visible());

            for status in [
                RoomSearchStatus::Idle,
                RoomSearchStatus::Searching,
                RoomSearchStatus::Failed,
            ] {
                strip.render(Some(&snapshot(status, vec![])));
                assert!(strip.root.is_visible());
                assert!(
                    rows(&strip).is_empty(),
                    "{status:?} paints no result region"
                );
            }

            strip.render(Some(&snapshot(RoomSearchStatus::Done, vec![])));
            assert_eq!(rows(&strip).len(), 1);
            assert!(
                crate::testid::find_by_test_id(&strip.root, ids::ROOM_SEARCH_NO_RESULTS).is_some()
            );

            let post = RoomSearchNav::Post {
                post_id: "p1".into(),
                home_nest_url: Some("https://home.example".into()),
            };
            strip.render(Some(&snapshot(
                RoomSearchStatus::Done,
                vec![
                    hit(RoomSearchResultKind::Post, post.clone()),
                    hit(
                        RoomSearchResultKind::Post,
                        RoomSearchNav::Post {
                            post_id: "p2".into(),
                            home_nest_url: None,
                        },
                    ),
                ],
            )));
            let painted = rows(&strip);
            assert_eq!(painted.len(), 2, "the empty state leaves with the rows");
            painted[0]
                .clone()
                .downcast::<gtk::Button>()
                .expect("a row is a button")
                .emit_clicked();
            assert_eq!(*opened.borrow(), vec![post]);

            strip.render(None);
            assert!(!strip.root.is_visible());
            assert!(rows(&strip).is_empty(), "closing the strip drops its rows");
        });
    }
}
