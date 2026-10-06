//! Compose bar at the bottom of the detail pane (and reused in new-thread
//! compose). Body field, optional subject input, reply preview, markdown
//! toolbar, attachment + send buttons.
//!
//! Mirrors `apps/fauna-windows/.../Controls/DmComposeBar.xaml{,.cs}`.
//! Capability gating: render every affordance unconditionally, gate
//! `set_sensitive`/`set_visible` on `caps.*`.
//!
//! All state lives in the shared `ComposeState`; this widget renders it
//! and forwards keystroke / click events to the parent's wired callbacks
//! (which call `manager.set_compose_body` / `toggle_topic` / `set_*` etc.).

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

use fauna_conversations::{
    Rail, ThreadCapabilities, TypedAddress,
    compose::{AttachmentDraft, ComposeState},
};

use super::{compose_decoration, compose_toolbar};

type RemoveRecipientCb = Rc<RefCell<Option<Box<dyn Fn(TypedAddress)>>>>;
type RemoveAttachmentCb = Rc<RefCell<Option<Box<dyn Fn(u32)>>>>;
type StringCb = Rc<RefCell<Option<Box<dyn Fn(String)>>>>;
type VoidCb = Rc<RefCell<Option<Box<dyn Fn()>>>>;

pub struct ComposeBar {
    pub root: gtk::Box,
    text_view: gtk::TextView,
    subject_input: gtk::Entry,
    markdown_toolbar: gtk::Box,
    reply_banner: gtk::Box,
    reply_preview: gtk::Label,
    /// Editable reply "To" line (`conversations.md` § Participants vs reply
    /// recipients) — visible only on rails with `supports_recipient_selection`.
    reply_to_box: gtk::Box,
    reply_chips_box: gtk::Box,
    /// Staged-attachment chip row — one removable chip per `compose.attachments`
    /// entry, rendered above the body field regardless of capability gating
    /// (whatever's staged shows; the attach *button* is what's capability-gated).
    attachment_chips_box: gtk::Box,
    topic_btn: gtk::ToggleButton,
    attach_btn: gtk::Button,
    send_btn: gtk::Button,
    refreshing: Rc<RefCell<bool>>,
    on_body_change: StringCb,
    on_subject_change: StringCb,
    on_topic_toggle: VoidCb,
    on_send: VoidCb,
    on_attach: VoidCb,
    on_reply_cancel: VoidCb,
    on_remove_reply_recipient: RemoveRecipientCb,
    on_add_reply_recipient: StringCb,
    on_remove_attachment: RemoveAttachmentCb,
}

impl ComposeBar {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_margin_start(8);
        root.set_margin_end(8);
        root.set_margin_top(4);
        root.set_margin_bottom(8);

        // Reply banner (collapsed unless reply_to is set).
        let reply_banner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        reply_banner.set_visible(false);
        let reply_icon = gtk::Image::from_icon_name("mail-reply-sender-symbolic");
        reply_icon.set_pixel_size(16);
        reply_banner.append(&reply_icon);
        let reply_preview = gtk::Label::new(None);
        reply_preview.set_hexpand(true);
        reply_preview.set_halign(gtk::Align::Start);
        reply_preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
        crate::testid::set_test_id(&reply_preview, ids::DM_REPLY_PREVIEW);
        reply_banner.append(&reply_preview);
        let reply_cancel = gtk::Button::from_icon_name("window-close-symbolic");
        reply_cancel.add_css_class("flat");
        crate::testid::set_test_id(&reply_cancel, ids::DM_REPLY_CANCEL);
        reply_banner.append(&reply_cancel);
        root.append(&reply_banner);

        // Editable reply "To" line (mail only — gated on
        // supports_recipient_selection in render). "To:" label | chips | add
        // input. Each chip carries a removable recipient; the entry appends one
        // (`conversations.md` § Participants vs reply recipients).
        let reply_to_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        reply_to_box.set_visible(false);
        let to_label = gtk::Label::new(Some(
            crate::i18n::strings::conversations::unified::TO_LINE_LABEL,
        ));
        to_label.add_css_class("dim-label");
        reply_to_box.append(&to_label);
        let reply_chips_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        reply_to_box.append(&reply_chips_box);
        let reply_add_entry = gtk::Entry::new();
        reply_add_entry.set_placeholder_text(Some(
            crate::i18n::strings::conversations::unified::REPLY_RECIPIENT_ADD_PLACEHOLDER,
        ));
        reply_add_entry.set_hexpand(true);
        crate::testid::set_test_id(&reply_add_entry, ids::DM_REPLY_RECIPIENT_ADD);
        reply_to_box.append(&reply_add_entry);
        root.append(&reply_to_box);

        // Staged attachments — one removable chip per `compose.attachments`
        // entry (`conversations.md` § Attachments → Staged-attachment
        // preview). Rendered above the body field so what is attached reads
        // as part of the message being written. Mirrors apple's
        // `attachmentChips` (`DmComposeBar.swift:274-310`) and this bar's own
        // `reply_chips_box` idiom just above.
        let attachment_chips_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        attachment_chips_box.set_visible(false);
        root.append(&attachment_chips_box);

        // Subject input (collapsed unless compose.subject_draft.is_some()).
        let subject_input = gtk::Entry::new();
        subject_input.set_placeholder_text(Some(
            crate::i18n::strings::conversations::unified::TOPIC_INPUT_PLACEHOLDER,
        ));
        subject_input.set_visible(false);
        crate::testid::set_test_id(&subject_input, ids::SUBJECT_INPUT);
        root.append(&subject_input);

        // Body text view.
        let text_view = gtk::TextView::new();
        text_view.set_wrap_mode(gtk::WrapMode::Word);
        text_view.set_top_margin(8);
        text_view.set_bottom_margin(8);
        text_view.set_left_margin(8);
        text_view.set_right_margin(8);
        crate::testid::set_test_id(&text_view, ids::DM_TEXT_FIELD);

        // Per-editor marker-visibility toggle state (default hidden — the new compose default;
        // tracked internally). Shared between the toolbar
        // toggle button and the decoration `apply` below.
        let markers_shown = Rc::new(Cell::new(false));

        // Markdown toolbar (existing helper) + the marker toggle.
        let markdown_toolbar =
            compose_toolbar::build_compact_toolbar(&text_view, markers_shown.clone());
        root.append(&markdown_toolbar);
        compose_toolbar::setup_compose_shortcuts(&text_view);

        // Bound the body scroller. `dm-text-field` is a multiline body
        // (`conversations.md` § detail), but a GtkScrolledWindow ignores its
        // child's `size_request` in the scroll direction, so without an
        // explicit min/max content height + natural-height propagation the
        // body (a) collapses to button height — the "far too small" composer —
        // and (b) oscillates height-for-width on every keystroke re-render,
        // which the `vexpand` messages list above absorbs by jumping size.
        // Clamp to 80–240px: a sensible multi-line default that grows with
        // content up to ~10 lines, then scrolls. Stable height ⇒ no jitter.
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .min_content_height(80)
            .max_content_height(240)
            .propagate_natural_height(true)
            .child(&text_view)
            .hexpand(true)
            .build();

        // Bottom row: topic-toggle | attach | scroll body | send
        let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 6);

        let topic_btn = gtk::ToggleButton::with_label(
            crate::i18n::strings::conversations::unified::TOPIC_TOGGLE_ADD,
        );
        topic_btn.add_css_class("flat");
        // Keep the action buttons button-sized and on the composer's bottom
        // baseline now that the body row is ≥80px tall (default Fill would
        // stretch them); matches `send_btn`'s `Align::End`.
        topic_btn.set_valign(gtk::Align::End);
        crate::testid::set_test_id(&topic_btn, ids::TOPIC_TOGGLE_BUTTON);
        bottom.append(&topic_btn);

        let attach_btn = gtk::Button::from_icon_name("mail-attachment-symbolic");
        attach_btn.add_css_class("flat");
        attach_btn.set_tooltip_text(Some(
            crate::i18n::strings::conversations::unified::ATTACHMENT_BUTTON,
        ));
        attach_btn.set_valign(gtk::Align::End);
        crate::testid::set_test_id(&attach_btn, ids::ATTACHMENT_BUTTON);
        bottom.append(&attach_btn);

        bottom.append(&scroll);

        let send_btn = gtk::Button::with_label(crate::i18n::strings::common::SEND);
        send_btn.add_css_class("suggested-action");
        send_btn.set_valign(gtk::Align::End);
        crate::testid::set_test_id(&send_btn, ids::DM_SEND_BUTTON);
        bottom.append(&send_btn);

        root.append(&bottom);

        let refreshing = Rc::new(RefCell::new(false));
        let on_body_change: StringCb = Rc::new(RefCell::new(None));
        let on_subject_change: StringCb = Rc::new(RefCell::new(None));
        let on_topic_toggle: VoidCb = Rc::new(RefCell::new(None));
        let on_send: VoidCb = Rc::new(RefCell::new(None));
        let on_attach: VoidCb = Rc::new(RefCell::new(None));
        let on_reply_cancel: VoidCb = Rc::new(RefCell::new(None));
        let on_remove_reply_recipient: RemoveRecipientCb = Rc::new(RefCell::new(None));
        let on_add_reply_recipient: StringCb = Rc::new(RefCell::new(None));
        let on_remove_attachment: RemoveAttachmentCb = Rc::new(RefCell::new(None));

        // Wire body buffer changes.
        {
            let cb = on_body_change.clone();
            let refreshing = refreshing.clone();
            text_view.buffer().connect_changed(move |buf| {
                if *refreshing.borrow() {
                    return;
                }
                if let Some(f) = cb.borrow().as_ref() {
                    // `true` = include hidden chars: the hide-by-default markdown mode tags
                    // inline markers `md-hidden` (invisible), but the draft forwarded to the
                    // manager (persisted + sent) MUST be the full markdown SOURCE, not the
                    // marker-stripped visible text — `false` would drop every concealed `**`.
                    let text = buf
                        .text(&buf.start_iter(), &buf.end_iter(), true)
                        .to_string();
                    f(text);
                }
            });
        }

        // Inline markdown styling (conversations.md § Compose-field inline markdown
        // styling): re-decorate on every text change *and* every caret move (the active
        // line's markers are revealed). Independent of the body-state callback above — it
        // runs even during programmatic `render()` updates (which clear the buffer's tags),
        // and is re-entrancy-safe (tag ops emit no `changed`/`cursor-position`).
        {
            let buffer = text_view.buffer();
            let ms = markers_shown.clone();
            buffer.connect_changed(move |buf| compose_decoration::apply(buf, ms.get()));
            let ms = markers_shown.clone();
            buffer.connect_cursor_position_notify(move |buf| {
                compose_decoration::apply(buf, ms.get());
            });
        }

        // Subject changes.
        {
            let cb = on_subject_change.clone();
            let refreshing = refreshing.clone();
            subject_input.connect_changed(move |entry| {
                if *refreshing.borrow() {
                    return;
                }
                if let Some(f) = cb.borrow().as_ref() {
                    f(entry.text().to_string());
                }
            });
        }

        // Topic toggle.
        {
            let cb = on_topic_toggle.clone();
            let refreshing = refreshing.clone();
            topic_btn.connect_toggled(move |_| {
                if *refreshing.borrow() {
                    return;
                }
                if let Some(f) = cb.borrow().as_ref() {
                    f();
                }
            });
        }

        // Send.
        {
            let cb = on_send.clone();
            send_btn.connect_clicked(move |_| {
                if let Some(f) = cb.borrow().as_ref() {
                    f();
                }
            });
        }

        // Attach.
        {
            let cb = on_attach.clone();
            attach_btn.connect_clicked(move |_| {
                if let Some(f) = cb.borrow().as_ref() {
                    f();
                }
            });
        }

        // Reply cancel.
        {
            let cb = on_reply_cancel.clone();
            reply_cancel.connect_clicked(move |_| {
                if let Some(f) = cb.borrow().as_ref() {
                    f();
                }
            });
        }

        // Add a reply recipient: Enter commits the typed address. The manager
        // parses + dedups; on success the snapshot re-renders the chip row.
        {
            let cb = on_add_reply_recipient.clone();
            reply_add_entry.connect_activate(move |entry| {
                let text = entry.text().to_string();
                if text.trim().is_empty() {
                    return;
                }
                if let Some(f) = cb.borrow().as_ref() {
                    f(text);
                }
                entry.set_text("");
            });
        }

        Self {
            root,
            text_view,
            subject_input,
            markdown_toolbar,
            reply_banner,
            reply_preview,
            reply_to_box,
            reply_chips_box,
            attachment_chips_box,
            topic_btn,
            attach_btn,
            send_btn,
            refreshing,
            on_body_change,
            on_subject_change,
            on_topic_toggle,
            on_send,
            on_attach,
            on_reply_cancel,
            on_remove_reply_recipient,
            on_add_reply_recipient,
            on_remove_attachment,
        }
    }

    /// `rail` is the composer's own send rail — the open thread's `ThreadDetail::rail`
    /// for the detail reply bar, or the new-thread composer's first committed
    /// recipient chip's rail (`None` before any chip is committed) — carried
    /// **for the offline gate only** (`send_btn` below), on the same terms as
    /// tui's `Action::SendThread`/`SendNewThread::rail`: the manager re-derives
    /// the real routing regardless, so a carried rail that disagreed could only
    /// mis-gate, never mis-route.
    pub fn render(
        &self,
        compose: &ComposeState,
        caps: Option<&ThreadCapabilities>,
        rail: Option<Rail>,
        // What the reply banner says — the shared `ConversationsManager::reply_preview`
        // (the answered message's sender + plain-text excerpt), never derived here.
        reply_preview: Option<&fauna_conversations::ReplyPreview>,
    ) {
        *self.refreshing.borrow_mut() = true;

        // Body — only update if diverges (avoid cursor jump). `true` = include hidden chars:
        // the draft is the full markdown source, so the diff must compare against the full
        // buffer source (incl. the `md-hidden`-concealed inline markers), not the visible
        // text — else hide-mode spuriously diverges every render and clobbers the buffer.
        let buf = self.text_view.buffer();
        let current = buf
            .text(&buf.start_iter(), &buf.end_iter(), true)
            .to_string();
        if current != compose.body_draft {
            buf.set_text(&compose.body_draft);
        }

        // Subject input — show iff Some(_); pre-fill text.
        if let Some(s) = &compose.subject_draft {
            self.subject_input.set_visible(true);
            if self.subject_input.text().as_str() != s.as_str() {
                self.subject_input.set_text(s);
            }
            self.topic_btn.set_active(true);
        } else {
            self.subject_input.set_visible(false);
            self.topic_btn.set_active(false);
        }

        // Reply preview banner.
        // It painted the bare message id until 2026-09-21; an answered message
        // outside the fetched window previews empty rather than stale or wrong.
        if compose.reply_to.is_some() {
            self.reply_banner.set_visible(true);
            let text = reply_preview
                .map(|p| format!("{}: {}", p.sender_display, p.excerpt))
                .unwrap_or_default();
            self.reply_preview.set_text(&text);
        } else {
            self.reply_banner.set_visible(false);
        }

        // Editable reply "To" line — visible only on recipient-selection rails
        // (mail). Rebuild the chip row from compose.reply_recipients; each chip
        // is removable (× → on_remove_reply_recipient), the trailing entry adds.
        let show_to_line = caps
            .map(|c| c.supports_recipient_selection)
            .unwrap_or(false);
        self.reply_to_box.set_visible(show_to_line);
        if show_to_line {
            while let Some(child) = self.reply_chips_box.first_child() {
                self.reply_chips_box.remove(&child);
            }
            for addr in &compose.reply_recipients {
                let cb = self.on_remove_reply_recipient.clone();
                self.reply_chips_box
                    .append(&build_reply_recipient_chip(addr, cb));
            }
        }

        // Staged-attachment chips — rebuilt from compose.attachments every
        // render, same shape as the reply-recipient chip row above. Always
        // visible when non-empty regardless of `caps`: the attach *button* is
        // what's capability-gated, not what's already staged.
        self.attachment_chips_box
            .set_visible(!compose.attachments.is_empty());
        while let Some(child) = self.attachment_chips_box.first_child() {
            self.attachment_chips_box.remove(&child);
        }
        for (index, draft) in compose.attachments.iter().enumerate() {
            let cb = self.on_remove_attachment.clone();
            self.attachment_chips_box
                .append(&build_attachment_chip(draft, index as u32, cb));
        }

        if let Some(caps) = caps {
            self.attach_btn.set_sensitive(caps.supports_attachments);
            self.topic_btn.set_sensitive(caps.supports_subject);
            // Gate the markdown toolbar Box (so the whole row greys out and
            // the e2e `get_attr("markdown-toolbar", "disabled")` reads
            // "true"), and each markdown button individually (so
            // `get_attr("markdown-bold-button", "disabled")` is correct too).
            self.markdown_toolbar.set_sensitive(caps.supports_markdown);
            let mut btn = self.markdown_toolbar.first_child();
            while let Some(w) = btn {
                if let Some(button) = w.downcast_ref::<gtk::Button>() {
                    button.set_sensitive(caps.supports_markdown);
                }
                btn = w.next_sibling();
            }
        }

        // `dm-send-button` — declared fresh every render off the CURRENT rail,
        // through the shared `Rail::send_wire_kind` (the one answer both
        // composers on both apps read): every rail
        // resolves to a real kind, so `render` (called on every observer tick —
        // see the doc comment above) re-declares constantly and a stale kind
        // from a previously-viewed thread's rail cannot outlive the next tick.
        // `rail: None` (no chip committed
        // yet on the new-thread composer) declares nothing (rule 3 — no
        // wire call, no kind) — same known "cannot retract an earlier
        // declaration" gap as `add_participant_btn` in `thread_header.rs`, not
        // fixed here (needs an `offline_gate.rs` change, out of this sweep's
        // scope).
        if let Some(rail) = rail
            && let Some(kind) = rail.send_wire_kind()
        {
            crate::offline_gate::declare_wire_kind(&self.send_btn, kind);
        }

        *self.refreshing.borrow_mut() = false;
    }

    pub fn on_body_change(&self, f: impl Fn(String) + 'static) {
        *self.on_body_change.borrow_mut() = Some(Box::new(f));
    }
    pub fn on_subject_change(&self, f: impl Fn(String) + 'static) {
        *self.on_subject_change.borrow_mut() = Some(Box::new(f));
    }
    pub fn on_topic_toggle(&self, f: impl Fn() + 'static) {
        *self.on_topic_toggle.borrow_mut() = Some(Box::new(f));
    }
    pub fn on_send(&self, f: impl Fn() + 'static) {
        *self.on_send.borrow_mut() = Some(Box::new(f));
    }
    pub fn on_attach(&self, f: impl Fn() + 'static) {
        *self.on_attach.borrow_mut() = Some(Box::new(f));
    }
    pub fn on_reply_cancel(&self, f: impl Fn() + 'static) {
        *self.on_reply_cancel.borrow_mut() = Some(Box::new(f));
    }
    /// Fired when a reply-recipient chip's × is clicked — wired to
    /// `manager.remove_reply_recipient`.
    pub fn on_remove_reply_recipient(&self, f: impl Fn(TypedAddress) + 'static) {
        *self.on_remove_reply_recipient.borrow_mut() = Some(Box::new(f));
    }
    /// Fired with the raw typed text when the add-recipient entry is committed —
    /// wired to parse + `manager.add_reply_recipient`.
    pub fn on_add_reply_recipient(&self, f: impl Fn(String) + 'static) {
        *self.on_add_reply_recipient.borrow_mut() = Some(Box::new(f));
    }
    /// Fired with the attachment's index when a staged chip's × is clicked —
    /// wired to `manager.remove_attachment`/`remove_new_thread_attachment`.
    pub fn on_remove_attachment(&self, f: impl Fn(u32) + 'static) {
        *self.on_remove_attachment.borrow_mut() = Some(Box::new(f));
    }
}

/// A removable chip on the reply "To" line: pill with the address label
/// (`dm-reply-recipient-chip`) + a × button (`dm-reply-recipient-remove`).
/// Test ids sit on the inner Label/Button per the windows AT-SPI lesson.
fn build_reply_recipient_chip(addr: &TypedAddress, on_remove: RemoveRecipientCb) -> gtk::Box {
    let chip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    chip.add_css_class("pill");

    let label = gtk::Label::new(Some(&addr.display()));
    label.set_margin_start(8);
    label.set_margin_top(2);
    label.set_margin_bottom(2);
    crate::testid::set_test_id(&label, ids::DM_REPLY_RECIPIENT_CHIP);
    chip.append(&label);

    let remove = gtk::Button::from_icon_name("window-close-symbolic");
    remove.add_css_class("flat");
    remove.set_margin_end(2);
    crate::testid::set_test_id(&remove, ids::DM_REPLY_RECIPIENT_REMOVE);
    let addr = addr.clone();
    remove.connect_clicked(move |_| {
        if let Some(f) = on_remove.borrow().as_ref() {
            f(addr.clone());
        }
    });
    chip.append(&remove);

    chip
}

/// A removable chip for a staged compose attachment: an icon distinguishing
/// image vs. other files, a label naming the file and its size
/// (`dm-compose-attachment-chip`), and a × button
/// (`dm-compose-attachment-remove`). `index` is positional over
/// `ComposeState.attachments` — the same list the caller's `enumerate()`
/// loop walks — so the chip and its remove mutator's index cannot drift.
/// The size label routes through the shared `fauna_core::format::byte_size`
/// (`crate::i18n::byte_size`), never a hand-rolled threshold table
/// (`value-formatting.md` § Byte sizes, priority #2).
fn build_attachment_chip(
    draft: &AttachmentDraft,
    index: u32,
    on_remove: RemoveAttachmentCb,
) -> gtk::Box {
    let chip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    chip.add_css_class("pill");

    let icon_name = if draft.is_image {
        "image-x-generic-symbolic"
    } else {
        "text-x-generic-symbolic"
    };
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_margin_start(8);
    chip.append(&icon);

    let text = format!(
        "{}  {}",
        draft.filename,
        crate::i18n::byte_size(draft.size_bytes)
    );
    let label = gtk::Label::new(Some(&text));
    label.set_margin_top(2);
    label.set_margin_bottom(2);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    crate::testid::set_test_id(&label, ids::DM_COMPOSE_ATTACHMENT_CHIP);
    chip.append(&label);

    let remove = gtk::Button::from_icon_name("window-close-symbolic");
    remove.add_css_class("flat");
    remove.set_margin_end(2);
    crate::testid::set_test_id(&remove, ids::DM_COMPOSE_ATTACHMENT_REMOVE);
    remove.connect_clicked(move |_| {
        if let Some(f) = on_remove.borrow().as_ref() {
            f(index);
        }
    });
    chip.append(&remove);

    chip
}
