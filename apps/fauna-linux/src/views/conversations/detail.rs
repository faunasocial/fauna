//! Detail pane: a 3-state `gtk::Stack` (empty | thread | new-thread).
//!
//! - `empty` (default): hint label, shown until a thread is selected.
//! - `thread`: ThreadHeader + ScrolledWindow{messages_box} + ComposeBar.
//! - `new_thread`: RecipientPicker + group hint + ComposeBar (separate
//!   instance so the user's draft for the new compose doesn't share state
//!   with any selected thread's draft).
//!
//! `render` switches via `set_visible_child_name` based on whether
//! `snapshot.new_thread_compose.is_some()` (→ `new_thread`) and whether
//! `snapshot.selected_thread_id.is_some()` (→ `thread` else `empty`).
//! The messages stream is identity-keyed and reconciled imperatively, mirroring
//! windows' `RefreshDetailView`/`ReconcileChildren` (render-model.md §
//! Implementation status today) — a message whose content and selection state
//! are both unchanged gets no widget touched. Subject dividers render BEFORE
//! bubbles whose `subject_line.is_some()`.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;

use fauna_conversations::{
    ConversationsManager, SendState,
    message::MessageId,
    snapshot::{ConversationsSnapshot, ThreadDetail},
};

use crate::client::FaunaClient;
use crate::settings::member_review::Roster;

use super::{
    add_participant_overlay, compose_bar::ComposeBar, message_bubble,
    recipient_picker::RecipientPicker, rename_overlay, room_settings_overlay, subject_divider,
    thread_header::ThreadHeader,
};

pub struct ConversationDetail {
    pub root: gtk::Box,
    stack: gtk::Stack,
    /// Page-level error surface (`conversations.md` § Errors & edge cases:
    /// `error-message`). Observer-driven: `render` sets it from the active
    /// compose's `send_state == Failed { reason }` so a failed Send isn't
    /// swallowed (rule 1 — no client-side state machine).
    error_label: gtk::Label,
    // ── Thread page ────────────────────────────────────────────
    thread_header: ThreadHeader,
    messages_box: gtk::Box,
    messages_scroll: gtk::ScrolledWindow,
    /// The thread whose bubbles/dividers/snapshots the maps below hold —
    /// cleared and the messages stream rebuilt from scratch only on a thread
    /// switch, never on an in-thread message change. `render` runs on *every*
    /// observer tick, including every compose-body keystroke, so without the
    /// identity-keyed reuse below a bare tick used to tear down and rebuild
    /// every bubble: that flickered each `TextView` through GTK's first-
    /// measure (too-tall) → wrapped height, re-pinned the view to the bottom
    /// on every keypress, and dropped any open ⋯ popover / delete-confirm or
    /// body text selection the instant an unrelated message changed. Mirrors
    /// windows' `_bubblesThreadId`.
    bubbles_thread_id: RefCell<Option<fauna_conversations::ThreadId>>,
    /// One bubble widget per message id, reused across renders — rebound in
    /// place ([`message_bubble::build`]'s `existing` param) when that
    /// message's own content or selection state changed, otherwise left
    /// completely untouched. web (`{#each … (msg.message_id)}`) and android
    /// (`key = { it.messageId }`) key their message lists the same way;
    /// windows' `_bubblesById` is the identity-keyed twin this mirrors
    /// (render-model.md § Implementation status today).
    bubbles_by_id: RefCell<HashMap<MessageId, gtk::Box>>,
    /// Subject dividers, keyed and reused the same way (windows `_dividersById`).
    dividers_by_id: RefCell<HashMap<MessageId, gtk::Box>>,
    /// The newest `MessageSnapshot` per message id, updated every render tick
    /// regardless of whether that message's bubble needed a rebuild this
    /// tick — both the per-message change check in `render` and a reused
    /// bubble's event closures (`message_bubble::build`) read it, so a
    /// closure never acts on a stale render-time capture (windows
    /// `_latestSnapshotById`).
    latest_snapshot_by_id: message_bubble::LatestSnapshotById,
    /// Which of each message's attachments had resident bytes when its bubble
    /// was last built, in document order. Evicting bytes or fetching them again
    /// changes no message, so a bubble keyed on the message alone kept painting
    /// what it painted before — a dropped picture stayed painted, and one fetched
    /// again stayed a placeholder (`conversations.md` § Attachments →
    /// *Retention*). The residency is part of the change check below.
    attachment_residency_by_id: RefCell<HashMap<MessageId, Vec<bool>>>,
    /// `selected_message_id` as of the last render — lets the per-message
    /// change check catch a pure selection move (no content changed) so the
    /// old and new selected bubbles still repaint their `message-selected`
    /// class / `selected` timestamp attribute.
    selected_message_id_rendered: RefCell<Option<MessageId>>,
    /// The `SearchNav::Mail` selected message already brought into view —
    /// once per selection, never per tick, so a per-tick bring-into-view
    /// never fights the user's own scrolling (windows
    /// `_broughtIntoViewMessageId`).
    brought_into_view_message_id: RefCell<Option<MessageId>>,
    detail_compose: ComposeBar,
    /// Active thread id — closures borrow this via Rc, render() updates it.
    detail_thread_id: std::rc::Rc<std::cell::RefCell<Option<fauna_conversations::ThreadId>>>,
    /// Per-message reply seeder, passed into each `message_bubble::build`
    /// (`dm-reply-button` / `dm-reply-all-button` → `manager.start_reply`).
    on_reply: message_bubble::OnReply,
    /// tokio runtime handle, passed into each `message_bubble::build` to drive the
    /// on-demand remote-image fetch when a blocked inbound image is revealed.
    rt: tokio::runtime::Handle,
    /// App nest client, passed into each `message_bubble::build` to load a
    /// link-preview card's og:image blob (render-model.md § D4 — the shared walker
    /// has no blob loader; the same async-byte-load-stays-client idiom as the feed).
    client: Rc<FaunaClient>,
    /// The post-succession member-review roster the chip pair reads
    /// (`identity-succession.md` § Propagation → *MLS groups*, item 3a) — this
    /// window's one app-wide copy, shared with the contacts badge and never
    /// read by this pane itself (`settings::member_review::Roster`).
    member_reviews: Rc<Roster>,
    // ── New-thread page ───────────────────────────────────────
    new_thread_picker: RecipientPicker,
    new_thread_compose: ComposeBar,
    group_hint: gtk::Label,
    // ── Add-participant overlay ───────────────────────────────
    add_participant_dialog: adw::MessageDialog,
    add_participant_picker: RecipientPicker,
    add_participant_visible: std::cell::Cell<bool>,
}

impl ConversationDetail {
    pub fn new(
        manager: Arc<ConversationsManager>,
        client: Rc<FaunaClient>,
        rt: tokio::runtime::Handle,
        member_reviews: Rc<Roster>,
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(120);
        stack.set_vexpand(true);

        // ── empty page ────────────────────────────────────────
        let empty = adw::StatusPage::builder()
            .title(crate::i18n::strings::conversations::list::SELECT_CONVERSATION)
            .icon_name("mail-unread-symbolic")
            .build();
        stack.add_named(&empty, Some("empty"));

        // ── thread page ───────────────────────────────────────
        let thread_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let thread_header = ThreadHeader::new();
        thread_box.append(&thread_header.root);

        let messages_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        messages_box.set_margin_top(8);
        messages_box.set_margin_bottom(8);
        messages_box.set_margin_start(8);
        messages_box.set_margin_end(8);
        let messages_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&messages_box)
            .build();
        thread_box.append(&messages_scroll);

        let detail_compose = ComposeBar::new();
        thread_box.append(&detail_compose.root);

        stack.add_named(&thread_box, Some("thread"));

        // ── new_thread page ───────────────────────────────────
        let new_thread_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Header row: a "New Message" title + Cancel/discard affordance. Cancel →
        // manager.cancel_new_conversation() clears the new-thread draft
        // (conversations.md § Persistence: only an explicit cancel/discard or a
        // successful send clears it — switching now *preserves* the draft). The
        // snapshot observer then switches the stack off `new_thread` (the page is
        // shown iff snapshot.new_thread_compose.is_some(), so a cleared draft
        // collapses the composer); re-opening via new-conversation-button seeds a
        // fresh empty one. Mirrors the windows reference (ConversationsPage.xaml
        // NewThreadView header row).
        let new_thread_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        new_thread_header.set_margin_top(8);
        new_thread_header.set_margin_start(8);
        new_thread_header.set_margin_end(8);
        new_thread_header.set_margin_bottom(4);
        let new_thread_title = gtk::Label::new(Some(
            crate::i18n::strings::conversations::compose::NEW_MESSAGE,
        ));
        new_thread_title.add_css_class("title-4");
        new_thread_title.set_halign(gtk::Align::Start);
        new_thread_title.set_hexpand(true);
        new_thread_header.append(&new_thread_title);

        let new_thread_cancel = gtk::Button::with_label(crate::i18n::strings::common::CANCEL);
        new_thread_cancel.add_css_class("flat");
        crate::testid::set_test_id(&new_thread_cancel, ids::NEW_CONVERSATION_CANCEL);
        {
            let m = manager.clone();
            new_thread_cancel.connect_clicked(move |_| {
                m.cancel_new_conversation();
            });
        }
        new_thread_header.append(&new_thread_cancel);
        new_thread_box.append(&new_thread_header);

        // RecipientPicker for the new thread — input wired to manager.
        // on_accept first runs the async backend probe (`resolve_recipient`,
        // which promotes a typed Fauna actor id to a `Fauna` chip via the
        // keypackage probe) on the tokio runtime, then commits the resolved
        // chip through accept_current_recipient_chip — the manager decides
        // which picker is active (add-participant takes priority).
        let new_thread_picker = {
            let m1 = manager.clone();
            let m2 = manager.clone();
            let rt1 = rt.clone();
            let rt2 = rt.clone();
            RecipientPicker::new(
                // Typing owes a probe: the sync write parks the picker on
                // `resolving`, and the manager's async resolve — spawned per
                // keystroke, the manager only stamps a result whose input is
                // still current — settles it (`conversations.md` § Errors &
                // edge cases → *The picker tells the truth*).
                move |text| {
                    m1.set_new_thread_recipient_input(text);
                    let m = m1.clone();
                    rt1.spawn(async move {
                        m.resolve_recipient().await;
                    });
                },
                move || {
                    let m = m2.clone();
                    rt2.spawn(async move {
                        m.resolve_recipient().await;
                        m.accept_current_recipient_chip();
                    });
                },
            )
        };
        new_thread_box.append(&new_thread_picker.root);

        let group_hint = gtk::Label::new(Some(
            crate::i18n::strings::conversations::unified::GROUP_CONVERSATION_HINT,
        ));
        group_hint.add_css_class("dim-label");
        group_hint.add_css_class("caption");
        group_hint.set_halign(gtk::Align::Start);
        group_hint.set_margin_start(8);
        group_hint.set_margin_top(4);
        crate::testid::set_test_id(&group_hint, ids::GROUP_CONVERSATION_HINT);
        // Pin Property::Label so the test id stays on the accessible name
        // (GtkLabel derives name from text, which equals the i18n hint).
        group_hint.update_property(&[gtk::accessible::Property::Label("group-conversation-hint")]);
        // Hidden by default; render() shows it when chips.len() >= 2 to
        // match the Windows reference (ConversationsPage.xaml.cs:227).
        group_hint.set_visible(false);
        new_thread_box.append(&group_hint);

        // Spacer.
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_vexpand(true);
        new_thread_box.append(&spacer);

        let new_thread_compose = ComposeBar::new();
        new_thread_box.append(&new_thread_compose.root);

        stack.add_named(&new_thread_box, Some("new_thread"));

        root.append(&stack);

        // Page-level error surface (`conversations.md` § Errors & edge cases:
        // `error-message`) — shows the active compose's `send_state ==
        // Failed { reason }` so a failed Send (e.g. the nest rejecting
        // `fauna.email.send` when mail isn't provisioned) is visible instead of
        // swallowed to stderr. Hidden until `render` shows it; updated
        // observer-driven from the snapshot.
        let error_label = gtk::Label::builder()
            .visible(false)
            .wrap(true)
            .xalign(0.0)
            .css_classes(["error"])
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(4)
            .build();
        crate::testid::set_test_id(&error_label, ids::ERROR_MESSAGE);
        root.append(&error_label);

        // Default to empty.
        stack.set_visible_child_name("empty");

        // ── compose-bar wiring (thread) ─────────────────────────────────
        // Parent pre-populates the active thread id whenever it renders;
        // closures hold an Rc<RefCell<Option<ThreadId>>> that tracks it.
        let detail_thread_id: std::rc::Rc<
            std::cell::RefCell<Option<fauna_conversations::ThreadId>>,
        > = std::rc::Rc::new(std::cell::RefCell::new(None));
        // Convention 17's verdict-side walk (`region::block_render_json`): the
        // open thread's messages the region blocks — a deleted or legally
        // withdrawn one paints its own tombstone ahead of the region arm
        // (`message_bubble::build`), as on tui.
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            crate::region::register_block_counter(&messages_box, move || {
                let Some(detail) = id.borrow().clone().and_then(|tid| m.thread_detail(tid)) else {
                    return 0;
                };
                detail
                    .messages
                    .iter()
                    .filter(|msg| !msg.deleted && msg.legal_takedown_ref.is_none())
                    .filter(|msg| {
                        crate::region::is_region_blocked(&crate::region::verdict_for(
                            &msg.message_id.0,
                            &msg.labels,
                            || crate::region::message_input(&msg.body),
                        ))
                    })
                    .count()
            });
        }
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            detail_compose.on_body_change(move |body| {
                if let Some(tid) = id.borrow().clone() {
                    m.set_compose_body(tid, body);
                }
            });
        }
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            detail_compose.on_subject_change(move |s| {
                if let Some(tid) = id.borrow().clone() {
                    m.set_compose_subject(tid, s);
                }
            });
        }
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            detail_compose.on_topic_toggle(move || {
                if let Some(tid) = id.borrow().clone() {
                    m.toggle_topic(tid);
                }
            });
        }
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            detail_compose.on_reply_cancel(move || {
                if let Some(tid) = id.borrow().clone() {
                    m.set_reply_to(tid, None);
                }
            });
        }
        // Editable reply "To" line (mail): remove a chip / add an address.
        // Both are sync manager mutations (no wire op — the recipient set is a
        // local draft until Send) (`conversations.md` § Participants vs reply
        // recipients).
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            detail_compose.on_remove_reply_recipient(move |addr| {
                if let Some(tid) = id.borrow().clone() {
                    m.remove_reply_recipient(tid, addr);
                }
            });
        }
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            detail_compose.on_add_reply_recipient(move |text| {
                if let Some(tid) = id.borrow().clone() {
                    // Shared format-only parse — same recognizer the recipient
                    // picker uses; a malformed entry is a no-op.
                    if let Some(addr) = fauna_conversations::try_parse_typed_address(&text) {
                        m.add_reply_recipient(tid, addr);
                    }
                }
            });
        }
        // Per-message reply seeder (`dm-reply-button` sender-only /
        // `dm-reply-all-button` reply-all → `manager.start_reply`). `start_reply`
        // is sync; the snapshot re-render shows the seeded To line.
        let on_reply: message_bubble::OnReply = {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            std::rc::Rc::new(move |msg_id, reply_all| {
                if let Some(tid) = id.borrow().clone() {
                    m.start_reply(tid, msg_id, reply_all);
                }
            })
        };
        // Send for an existing thread: route the per-thread draft through the
        // rail backend on the tokio runtime (the SMTP backend reaches the nest
        // over WS-RPC; the body is already kept in sync via on_body_change).
        // The resulting snapshot change (Idle/Failed + appended Sent copy)
        // re-renders via the observer.
        {
            let m = manager.clone();
            let rt = rt.clone();
            let id = detail_thread_id.clone();
            detail_compose.on_send(move || {
                let Some(tid) = id.borrow().clone() else {
                    return;
                };
                // No pre-send SMTP re-register: the rail reads the session's
                // live self-address cell at send time (seeded at AuthSuccess,
                // pushed by the `IdentityRefreshed` handler —
                // `conversations.md` § State & data shape → *Self-address:
                // live, never baked*).
                let m2 = m.clone();
                rt.spawn(async move {
                    if let Err(e) = m2.send(tid).await {
                        tracing::error!("send failed: {e}");
                    }
                });
            });
        }

        // Attach (existing thread): native file-chooser, mirroring feed's
        // `post_list.rs` attach button. Unlike feed's deferred
        // `stage_attachment`/`create_post` model, `add_attachment` needs the
        // bytes immediately (staged on the manager's compose draft right
        // away), so the file is read synchronously inside the dialog
        // callback rather than deferred to send time.
        // `docs/goal/ui/conversations.md` § Attachments.
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            let compose_root = detail_compose.root.clone();
            detail_compose.on_attach(move || {
                let Some(tid) = id.borrow().clone() else {
                    return;
                };
                let dialog = gtk::FileDialog::builder()
                    .title(crate::i18n::strings::conversations::unified::ATTACHMENT_BUTTON)
                    .build();
                let win = compose_root
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok());
                let m = m.clone();
                dialog.open(win.as_ref(), gio::Cancellable::NONE, move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        return;
                    };
                    let Ok(bytes) = std::fs::read(&path) else {
                        return;
                    };
                    let filename = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "file".into());
                    let mime = fauna_conversations::compose::guess_mime_type(&filename).to_string();
                    m.add_attachment(tid, filename, mime, bytes);
                });
            });
        }

        // Remove a staged attachment (existing thread) — the
        // `dm-compose-attachment-remove` chip × (`conversations.md` §
        // Attachments → Staged-attachment preview).
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            detail_compose.on_remove_attachment(move |index| {
                if let Some(tid) = id.borrow().clone() {
                    m.remove_attachment(tid, index);
                }
            });
        }

        // Rename + add-participant on the thread header.
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            let stack_for_dialog = stack.clone();
            let rt = rt.clone();
            thread_header.on_rename(move || {
                let Some(tid) = id.borrow().clone() else {
                    return;
                };
                let label = m
                    .thread_detail(tid.clone())
                    .map(|d| d.label)
                    .unwrap_or_default();
                let m_for_save = m.clone();
                let rt_for_save = rt.clone();
                rename_overlay::show(&stack_for_dialog, &label, move |new_label| {
                    // `rename_thread` is async (it fires the MLS NameChanged wire
                    // op for FaunaMls groups); drive it on the tokio runtime from
                    // this sync GTK closure, like the send button above.
                    let m2 = m_for_save.clone();
                    let tid2 = tid.clone();
                    rt_for_save.spawn(async move {
                        m2.rename_thread(tid2, new_label).await;
                    });
                });
            });
        }
        // `thread-room-settings-button` → the policy editor. Every rule the
        // editor obeys is the shared draft's; Save is one
        // `apply_room_settings` call, which stops at the first refusal — so
        // the editor closes only when all of them landed, and otherwise stays
        // open with the page's `error-message` saying which was refused
        // (`ui/conversations.md` § Element IDs, the `room_settings` sub-page).
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            let stack_for_dialog = stack.clone();
            let rt = rt.clone();
            thread_header.on_room_settings(move || {
                let Some(tid) = id.borrow().clone() else {
                    return;
                };
                let Some(detail) = m.thread_detail(tid.clone()) else {
                    return;
                };
                let m_for_save = m.clone();
                let rt_for_save = rt.clone();
                // Captured for the overlay's own index resolution; Save
                // below re-reads the LIVE list instead, so a member who left
                // while the overlay sat open cannot be written into the
                // room's signed policy.
                let participants = detail.participants.clone();
                let editor_slot: Rc<
                    RefCell<Option<Rc<room_settings_overlay::RoomSettingsEditor>>>,
                > = Rc::new(RefCell::new(None));
                let slot_for_save = Rc::clone(&editor_slot);
                let editor =
                    room_settings_overlay::show(&stack_for_dialog, &detail, move |staged| {
                        let live = m_for_save
                            .thread_detail(tid.clone())
                            .map(|d| d.participants)
                            .unwrap_or_else(|| participants.clone());
                        let edits = staged.edits(&live);
                        let slot = Rc::clone(&slot_for_save);
                        if edits.is_empty() {
                            // Nothing staged: Save is a close, not a commit.
                            if let Some(e) = slot.borrow().as_ref() {
                                e.close();
                            }
                            return;
                        }
                        let m2 = m_for_save.clone();
                        let tid2 = tid.clone();
                        // `apply_room_settings` is async (each edit is a policy
                        // commit on the channel), so the commits run on `rt` and
                        // only the verdict crosses back to the GTK thread — the
                        // `async_helper` discipline every other tokio-bound read
                        // on this app follows.
                        crate::async_helper::spawn_with_snapshot(
                            &rt_for_save,
                            move || async move {
                                let all_landed = m2.apply_room_settings(tid2, edits).await;
                                (all_landed, m2.page_error_diagnostic())
                            },
                            move |(all_landed, refusal)| {
                                let Some(e) = slot.borrow().clone() else {
                                    return;
                                };
                                if all_landed {
                                    e.close();
                                } else if let Some(message) = refusal {
                                    e.show_error(&message);
                                }
                            },
                        );
                    });
                *editor_slot.borrow_mut() = editor;
            });
        }

        // Build the persistent add-participant dialog + picker.
        let (add_participant_dialog, add_participant_picker) = add_participant_overlay::build(
            &root,
            {
                let m = manager.clone();
                let rt = rt.clone();
                // Same typing-owes-a-probe rule as the new-thread picker.
                move |text| {
                    m.set_add_participant_recipient_input(text);
                    let m2 = m.clone();
                    rt.spawn(async move {
                        m2.resolve_recipient().await;
                    });
                }
            },
            {
                let m = manager.clone();
                let rt = rt.clone();
                move || {
                    // Same resolve-then-accept flow as the new-thread picker:
                    // probe the typed recipient on the runtime, then commit the
                    // resolved chip.
                    let m2 = m.clone();
                    rt.spawn(async move {
                        m2.resolve_recipient().await;
                        m2.accept_current_recipient_chip();
                    });
                }
            },
            {
                let m = manager.clone();
                let rt = rt.clone();
                move || {
                    // `confirm_add_participant` is async (an in-place FaunaMls
                    // group add fires the MLS Commit + Welcome wire op); drive it
                    // on the tokio runtime from this sync GTK response handler.
                    let m2 = m.clone();
                    rt.spawn(async move {
                        m2.confirm_add_participant().await;
                    });
                }
            },
            {
                let m = manager.clone();
                move || m.cancel_add_participant()
            },
        );

        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            thread_header.on_add_participant(move || {
                if let Some(tid) = id.borrow().clone() {
                    m.open_add_participant(tid);
                }
            });
        }
        // Member-chip remove (`thread-member-chip[i] → manager.remove_participant`).
        // The header only attaches the click on membership-change-capable
        // threads; `remove_participant` is async (FaunaMls groups post an MLS
        // Commit), so drive it on the tokio runtime.
        {
            let m = manager.clone();
            let id = detail_thread_id.clone();
            let rt = rt.clone();
            thread_header.on_remove_member(move |addr| {
                let Some(tid) = id.borrow().clone() else {
                    return;
                };
                let m2 = m.clone();
                rt.spawn(async move {
                    m2.remove_participant(tid, addr).await;
                });
            });
        }

        // The chip pair's roster is this window's app-wide one
        // (`settings::member_review::Roster`) — shared with the contacts badge
        // and never read here: every refresh point lands through `app.rs`'s
        // `MemberReviewsLoaded` arm, including the aftermath's
        // `config_stage_settled` hook that raises a successor's roster
        // (`identity-succession.md` § Propagation → *MLS groups*, item 3a).
        // Repaint the open thread's chips the moment one lands: the pane
        // otherwise repaints only on its manager's snapshot ticks, and for a
        // successor the roster always lands after this pane was built — the
        // window is built at sign-in, the raise waits on a network re-seal.
        {
            let thread_header = thread_header.clone();
            let manager = manager.clone();
            let id = detail_thread_id.clone();
            member_reviews.connect_changed(move |reviews| {
                if let Some(tid) = id.borrow().clone()
                    && let Some(d) = manager.thread_detail(tid)
                {
                    thread_header.render(&d, reviews);
                }
            });
        }
        // Member-chip Keep (`thread-member-keep-button` → the shared
        // `fauna_client_config::decide_member_review` seam member_review.rs's
        // own settings-page Keep already calls), then the re-read that is the
        // surface's second ratified refresh point. The re-read goes through
        // `fetch_member_reviews` rather than straight into this pane, so the
        // roster's one writer lands it and the contacts badge drops the person
        // in the same pass as this chip. A failed Keep re-reads nothing — the
        // person is still open — and its failure reaches `error-message`.
        {
            let client = client.clone();
            let rt = rt.clone();
            thread_header.on_keep_member(move |person| {
                let client = client.clone();
                crate::async_helper::spawn_with_snapshot(
                    &rt,
                    move || async move { crate::settings::member_review::keep_person(person).await },
                    move |kept| match kept {
                        Ok(()) => client.fetch_member_reviews(),
                        Err(error) => client.tx().send(crate::app::UiMessage::Action(
                            crate::app::ActionResult::Failed {
                                context: "member_review_keep".into(),
                                error,
                            },
                        )),
                    },
                );
            });
        }

        // ── compose-bar wiring (new-thread) ──────────────────────────────
        {
            let m = manager.clone();
            new_thread_compose.on_body_change(move |body| {
                m.set_new_thread_body(body);
            });
        }
        {
            let m = manager.clone();
            new_thread_compose.on_subject_change(move |s| {
                m.set_new_thread_subject(if s.is_empty() { None } else { Some(s) });
            });
        }
        // Topic toggle for new-thread compose: there's no per-thread id, so
        // we toggle subject_draft directly via set_new_thread_subject.
        {
            let m = manager.clone();
            new_thread_compose.on_topic_toggle(move || {
                let snap = m.snapshot();
                let is_some = snap
                    .new_thread_compose
                    .as_ref()
                    .and_then(|c| c.subject_draft.as_ref())
                    .is_some();
                if is_some {
                    m.set_new_thread_subject(None);
                } else {
                    m.set_new_thread_subject(Some(String::new()));
                }
            });
        }
        // Send the new-thread compose: materialize the thread from the
        // recipient chips + draft and submit through the rail backend on the
        // tokio runtime.
        {
            let m = manager.clone();
            let rt = rt.clone();
            new_thread_compose.on_send(move || {
                // Same live-cell stance as the existing-thread send above.
                let m2 = m.clone();
                rt.spawn(async move {
                    if let Err(e) = m2.send_new_thread().await {
                        tracing::error!("send_new_thread failed: {e}");
                    }
                });
            });
        }

        // Attach (new-thread compose): same native file-chooser as the
        // existing-thread leg above, staging onto the single-slot new-thread
        // draft (`add_new_thread_attachment`); `send_new_thread` carries it
        // onto the materialized thread.
        {
            let m = manager.clone();
            let compose_root = new_thread_compose.root.clone();
            new_thread_compose.on_attach(move || {
                let dialog = gtk::FileDialog::builder()
                    .title(crate::i18n::strings::conversations::unified::ATTACHMENT_BUTTON)
                    .build();
                let win = compose_root
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok());
                let m = m.clone();
                dialog.open(win.as_ref(), gio::Cancellable::NONE, move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        return;
                    };
                    let Ok(bytes) = std::fs::read(&path) else {
                        return;
                    };
                    let filename = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "file".into());
                    let mime = fauna_conversations::compose::guess_mime_type(&filename).to_string();
                    m.add_new_thread_attachment(filename, mime, bytes);
                });
            });
        }

        // Remove a staged attachment (new-thread compose) — same chip × as
        // the existing-thread leg above, onto the single-slot new-thread
        // draft.
        {
            let m = manager.clone();
            new_thread_compose.on_remove_attachment(move |index| {
                m.remove_new_thread_attachment(index);
            });
        }

        Self {
            root,
            stack,
            error_label,
            thread_header,
            messages_box,
            messages_scroll,
            bubbles_thread_id: RefCell::new(None),
            bubbles_by_id: RefCell::new(HashMap::new()),
            dividers_by_id: RefCell::new(HashMap::new()),
            latest_snapshot_by_id: Rc::new(RefCell::new(HashMap::new())),
            attachment_residency_by_id: RefCell::new(HashMap::new()),
            selected_message_id_rendered: RefCell::new(None),
            brought_into_view_message_id: RefCell::new(None),
            detail_compose,
            detail_thread_id,
            on_reply,
            rt,
            client,
            member_reviews,
            new_thread_picker,
            new_thread_compose,
            group_hint,
            add_participant_dialog,
            add_participant_picker,
            add_participant_visible: std::cell::Cell::new(false),
        }
    }

    /// Render the detail pane from the current snapshot + active thread
    /// detail. Called by the orchestrator on every observer tick.
    pub fn render(
        &self,
        snapshot: &ConversationsSnapshot,
        active_detail: Option<&ThreadDetail>,
        self_handle: &str,
        served_elsewhere: bool,
        receive_stopped: bool,
        unopenable_mail: u32,
    ) {
        // Update the thread-id holder so compose-bar closures route mutators
        // to the current thread.
        *self.detail_thread_id.borrow_mut() = active_detail.map(|d| d.thread_id.clone());

        // Pick which stack page to show.
        if snapshot.new_thread_compose.is_some() {
            self.stack.set_visible_child_name("new_thread");
        } else if snapshot.selected_thread_id.is_some() {
            self.stack.set_visible_child_name("thread");
        } else {
            self.stack.set_visible_child_name("empty");
        }

        // ── new-thread page ───────────────────────────────────
        if let Some(c) = &snapshot.new_thread_compose {
            let chip_count = c
                .recipient_picker
                .as_ref()
                .map(|p| p.chips.len())
                .unwrap_or(0);
            if let Some(picker_state) = &c.recipient_picker {
                self.new_thread_picker
                    .render(picker_state, &snapshot.bridges);
            }
            // The offline gate's rail: the first committed recipient chip's
            // rail (`None` before any chip is committed) — `compose_bar.rs`'s
            // `render` doc comment.
            let new_thread_rail = c
                .recipient_picker
                .as_ref()
                .and_then(|p| p.chips.first())
                .and_then(|addr| addr.rail());
            // A new thread answers nothing, so there is no reply to preview.
            self.new_thread_compose
                .render(c, None, new_thread_rail, None);
            // Group-conversation hint surfaces once we have ≥2 chips — same
            // threshold as Windows ConversationsPage.xaml.cs:227.
            self.group_hint.set_visible(chip_count >= 2);
        } else {
            self.group_hint.set_visible(false);
        }

        // ── add-participant overlay ───────────────────────────
        // Show/hide the persistent dialog based on snapshot.add_participant.
        // Hidden GTK widgets are pruned from the AT-SPI tree so there's no
        // id collision with the new-thread picker while a thread is selected.
        match snapshot.add_participant.as_ref() {
            Some(state) => {
                self.add_participant_picker
                    .render(&state.picker, &snapshot.bridges);
                if !self.add_participant_visible.get() {
                    // The dialog is built eagerly in `new()`, before this
                    // detail widget is rooted, so — unlike the on-demand
                    // rename dialog (`rename_overlay::show`, which roots off
                    // the already-attached stack) — it has no transient parent
                    // yet. Anchor it to the toplevel the first time we present
                    // it, now that `root` is rooted; otherwise the dialog
                    // floats unanchored to the main window under Mutter.
                    if self.add_participant_dialog.transient_for().is_none()
                        && let Some(win) = self
                            .root
                            .root()
                            .and_then(|r| r.downcast::<gtk::Window>().ok())
                    {
                        self.add_participant_dialog.set_transient_for(Some(&win));
                    }
                    self.add_participant_dialog.present();
                    self.add_participant_picker.focus_input();
                    self.add_participant_visible.set(true);
                }
            }
            None => {
                if self.add_participant_visible.get() {
                    self.add_participant_dialog.close();
                    self.add_participant_visible.set(false);
                }
            }
        }

        // ── thread page ───────────────────────────────────────
        if let Some(d) = active_detail {
            self.thread_header
                .render(d, self.member_reviews.reviews().as_slice());
            // Identity-keyed message stream (render-model.md § Implementation
            // status today): reconcile `messages_box` to `d.messages` instead
            // of clearing and rebuilding every bubble on every tick — `render`
            // fires on every observer tick, including a bare compose-body
            // keystroke. A thread switch resets the maps below so nothing
            // stale is looked up; within a thread, only a message whose own
            // content or selection state changed gets a new/rebound widget —
            // everything else, including any open ⋯ popover/delete-confirm or
            // a body text selection, is left completely alone.
            let thread_switched = self.bubbles_thread_id.borrow().as_ref() != Some(&d.thread_id);
            if thread_switched {
                self.bubbles_by_id.borrow_mut().clear();
                self.dividers_by_id.borrow_mut().clear();
                self.latest_snapshot_by_id.borrow_mut().clear();
                self.attachment_residency_by_id.borrow_mut().clear();
                *self.selected_message_id_rendered.borrow_mut() = None;
                *self.brought_into_view_message_id.borrow_mut() = None;
                *self.bubbles_thread_id.borrow_mut() = Some(d.thread_id.clone());
            }
            let prev_selected = self.selected_message_id_rendered.borrow().clone();

            // Reply-all is offered only on recipient-selection rails (mail);
            // gated per bubble via the shared capability.
            let show_reply_all = d.capabilities.supports_recipient_selection;
            // The selected bubble — `SearchNav::Mail`'s second half
            // (conversations.md § The selected message).
            let mut selected_bubble: Option<gtk::Box> = None;
            // A message id absent from `bubbles_by_id` before this tick is a
            // genuinely NEW message (a reaction/delete/reveal never adds or
            // removes an id) — the signal that gates the bottom-scroll below,
            // distinct from an existing message's content changing.
            let mut appended_new_message = false;
            let mut seen: HashSet<MessageId> = HashSet::with_capacity(d.messages.len());
            let mut desired: Vec<gtk::Widget> = Vec::with_capacity(d.messages.len() * 2);
            for msg in &d.messages {
                seen.insert(msg.message_id.clone());
                let is_new = !self.bubbles_by_id.borrow().contains_key(&msg.message_id);
                appended_new_message |= is_new;

                let prev_snapshot = self
                    .latest_snapshot_by_id
                    .borrow()
                    .get(&msg.message_id)
                    .cloned();
                let content_changed = prev_snapshot.as_ref() != Some(msg);
                let subject_changed =
                    prev_snapshot.as_ref().map(|p| &p.subject_line) != Some(&msg.subject_line);
                self.latest_snapshot_by_id
                    .borrow_mut()
                    .insert(msg.message_id.clone(), msg.clone());

                let is_selected = d.selected_message_id.as_ref() == Some(&msg.message_id);
                let was_selected = prev_selected.as_ref() == Some(&msg.message_id);
                let residency: Vec<bool> =
                    fauna_conversations::message::attachment_blocks(&msg.document)
                        .into_iter()
                        .map(|a| crate::conversations::manager().attachment_resident(a.blob_hash))
                        .collect();
                let residency_changed = self
                    .attachment_residency_by_id
                    .borrow()
                    .get(&msg.message_id)
                    != Some(&residency);
                self.attachment_residency_by_id
                    .borrow_mut()
                    .insert(msg.message_id.clone(), residency);
                let changed =
                    is_new || content_changed || residency_changed || (is_selected != was_selected);

                if let Some(s) = &msg.subject_line {
                    if is_new
                        || subject_changed
                        || !self.dividers_by_id.borrow().contains_key(&msg.message_id)
                    {
                        let div = subject_divider::build(s);
                        self.dividers_by_id
                            .borrow_mut()
                            .insert(msg.message_id.clone(), div);
                    }
                } else {
                    self.dividers_by_id.borrow_mut().remove(&msg.message_id);
                }
                if let Some(div) = self.dividers_by_id.borrow().get(&msg.message_id) {
                    desired.push(div.clone().upcast());
                }

                if changed {
                    // Lazily resolve link previews — **fire-once** per `Resolving`
                    // block (render-model.md § D4, the conversations twin of the
                    // feed's `render_posts`) — only on a (re)build, since a bubble
                    // left untouched this tick has an unchanged document with
                    // nothing new to resolve. The shared
                    // `ConversationsManager::resolve_link_preview` calls
                    // `fauna.linkpreview.resolve`, folds the `Resolved` state into
                    // the message document + notifies (idempotently); the next
                    // change-triggered rebuild paints the card. A `Resolving` block
                    // is the fire-once guard (it disappears from this list once
                    // resolved), so the re-emit settles instead of looping.
                    for url in msg.document.resolving_link_preview_urls() {
                        let url = url.to_string();
                        self.rt.spawn(async move {
                            crate::conversations::manager()
                                .resolve_link_preview(url)
                                .await;
                        });
                    }
                    let existing = self.bubbles_by_id.borrow().get(&msg.message_id).cloned();
                    let bubble = message_bubble::build(
                        existing.as_ref(),
                        msg,
                        self_handle,
                        &d.capabilities,
                        &d.thread_id,
                        show_reply_all,
                        self.on_reply.clone(),
                        &self.client,
                        &self.rt,
                        is_selected,
                        &self.latest_snapshot_by_id,
                    );
                    self.bubbles_by_id
                        .borrow_mut()
                        .insert(msg.message_id.clone(), bubble.clone());
                    if is_selected {
                        selected_bubble = Some(bubble);
                    }
                } else if is_selected {
                    selected_bubble = self.bubbles_by_id.borrow().get(&msg.message_id).cloned();
                }
                if let Some(bubble) = self.bubbles_by_id.borrow().get(&msg.message_id) {
                    desired.push(bubble.clone().upcast());
                }
            }
            // A thread re-projection can drop a message id outright — prune it
            // from the caches; `reconcile_children` below drops it from the box.
            self.bubbles_by_id
                .borrow_mut()
                .retain(|id, _| seen.contains(id));
            self.dividers_by_id
                .borrow_mut()
                .retain(|id, _| seen.contains(id));
            self.latest_snapshot_by_id
                .borrow_mut()
                .retain(|id, _| seen.contains(id));
            self.attachment_residency_by_id
                .borrow_mut()
                .retain(|id, _| seen.contains(id));
            *self.selected_message_id_rendered.borrow_mut() = d.selected_message_id.clone();
            reconcile_children(&self.messages_box, &desired);

            // Bring a freshly-selected message into view once (never per tick
            // — conversations.md § The selected message, "bringing it into
            // view is part of the affordance, not a nicety" — a per-tick
            // bring-into-view would fight the user's own scrolling);
            // otherwise scroll to the bottom only on a thread switch or a
            // genuinely new message, never on a same-message-set content edit
            // (a reaction, a delete, a reveal). Both wait for the next layout
            // pass so `compute_bounds` sees the bubbles' real allocation.
            let selection_changed = self.brought_into_view_message_id.borrow().as_ref()
                != d.selected_message_id.as_ref();
            if let Some(bubble) = selected_bubble.filter(|_| selection_changed) {
                let scroll = self.messages_scroll.clone();
                gtk::glib::idle_add_local_once(move || {
                    scroll_widget_into_view(&scroll, &bubble);
                });
            } else if thread_switched || appended_new_message {
                let scroll = self.messages_scroll.clone();
                gtk::glib::idle_add_local_once(move || {
                    let adj = scroll.vadjustment();
                    adj.set_value(adj.upper() - adj.page_size());
                });
            }
            *self.brought_into_view_message_id.borrow_mut() = d.selected_message_id.clone();

            let reply_preview = crate::conversations::manager().reply_preview(d.thread_id.clone());
            self.detail_compose.render(
                &d.compose,
                Some(&d.capabilities),
                Some(d.rail),
                reply_preview.as_ref(),
            );
        } else {
            // Off the thread page (empty / new-thread) — drop the thread
            // marker so re-selecting ANY thread (even the same one) is
            // treated as a switch: full rebuild + bottom-scroll, mirroring
            // the old `rendered_msg_sig` reset.
            *self.bubbles_thread_id.borrow_mut() = None;
        }

        // ── page-level error surface ──────────────────────────
        // Surface either a failed membership/label op (`snapshot.error`) or a
        // failed compose-send (`conversations.md` § Errors & edge cases:
        // `error-message`) — see `page_error_text` for the precedence.
        let text = page_error_text(
            snapshot,
            active_detail,
            served_elsewhere,
            receive_stopped,
            unopenable_mail,
        );
        crate::settings::render_error_label(&self.error_label, text.as_deref());
    }
}

/// Precedence-resolve the page-level error text (`error-message`): the
/// standing conversations-engine-role refusal (`served_elsewhere` —
/// `ConversationsManager::engine_served_elsewhere`, `account-data-plane.md` §
/// Multi-instance concurrency) outranks every truth below — with no engine
/// there is no wire op to fail, and the honest "served in another instance"
/// must never be masked by an unrelated gesture's success, which is exactly
/// why it is a separate manager field rather than folded into
/// `snapshot.error` (that slot IS cleared on every producer's entry).
/// Directly under it, a dead receive rail (`receive_stopped` —
/// `ConversationsManager::receive_stopped`, `conversations.md` § Errors &
/// edge cases — "the receive rail stopped") is the same kind of standing
/// truth: a panicked receive loop is unknowable-safe to re-arm, so the notice
/// stands until a newer loop retires it, never cleared by a gesture either.
/// Below both, a failed membership/label op (`snapshot.error` —
/// `confirm_add_participant`/`remove_participant`/`rename_thread`) takes
/// precedence over a failed compose-send, mirroring tui's `sync_page_error`
/// (`apps/fauna-tui/src/conversations/mod.rs`) — every producer, sends
/// included, clears `snapshot.error` on entry, so it is always the more
/// recent of the two by construction. The active compose is the new-thread
/// compose when present, else the selected thread's.
///
/// Below all of it, the floor of the stack (`ui/conversations.md` § Errors &
/// edge cases → *A fifth truth*): `unopenable_mail` — received mail this run
/// the client could not open under the account's current key set
/// (`ConversationsManager::unopenable_mail_count`) — shows only when nothing
/// above it does, so a fresh failure of any other truth outranks it, and it is
/// never cleared by a gesture, only by the records opening on a later
/// re-drain. Pure (no widget access) so the precedence is unit-testable
/// without a live GTK tree.
fn page_error_text(
    snapshot: &ConversationsSnapshot,
    active_detail: Option<&ThreadDetail>,
    served_elsewhere: bool,
    receive_stopped: bool,
    unopenable_mail: u32,
) -> Option<String> {
    if served_elsewhere {
        return Some(crate::i18n::strings::conversations::errors::SERVED_ELSEWHERE.to_string());
    }
    if receive_stopped {
        return Some(crate::i18n::strings::conversations::errors::RECEIVE_STOPPED.to_string());
    }
    if let Some(lt) = &snapshot.error {
        return Some(lt.resolve(crate::i18n::strings::lookup));
    }
    let active_send_state = snapshot
        .new_thread_compose
        .as_ref()
        .map(|c| &c.send_state)
        .or_else(|| active_detail.map(|d| &d.compose.send_state));
    match active_send_state {
        // Both producers carry a `LocalizedText`, so both resolve through the
        // same pipeline — the send reason is not a pre-rendered English string.
        Some(SendState::Failed { reason }) => Some(reason.resolve(crate::i18n::strings::lookup)),
        _ if unopenable_mail > 0 => Some(
            crate::i18n::strings::conversations::errors::mail_unopenable(
                &unopenable_mail.to_string(),
            ),
        ),
        _ => None,
    }
}

/// Center `widget` in `scrolled`'s viewport by driving the real vadjustment —
/// the selected-message half of `conversations.md` § The selected message
/// ("bringing it into view is part of the affordance"). Also the production
/// half of the e2e-only `automation::agent::scroll_into_view` op: that module
/// is compiled out of release builds (e2e-conventions.md point 15), so it
/// calls back into this always-compiled fn rather than reimplementing the
/// math — the dependency can only run this direction. Returns `false` when
/// `widget`'s bounds relative to `scrolled` could not be computed (not laid
/// out / not a descendant).
pub(crate) fn scroll_widget_into_view(
    scrolled: &gtk::ScrolledWindow,
    widget: &impl IsA<gtk::Widget>,
) -> bool {
    let Some(bounds) = widget.compute_bounds(scrolled) else {
        return false;
    };
    let adj = scrolled.vadjustment();
    let target =
        adj.value() + f64::from(bounds.y()) - (adj.page_size() - f64::from(bounds.height())) / 2.0;
    adj.set_value(target.clamp(
        adj.lower(),
        (adj.upper() - adj.page_size()).max(adj.lower()),
    ));
    true
}

/// Make `container`'s children equal `desired` by identity and order, with
/// the fewest touches: a widget already at its spot is left alone (so its
/// live state — an open ⋯ popover, a text selection — survives), a stale one
/// is removed, a new or reordered one is (re)positioned. Mirrors windows'
/// `ReconcileChildren` — GTK's `Box` is a sibling-linked list rather than an
/// indexable collection, hence the sibling-walk + `insert_child_after`/
/// `reorder_child_after` form instead of `UIElementCollection`'s `Insert`/
/// `RemoveAt`.
fn reconcile_children(container: &gtk::Box, desired: &[gtk::Widget]) {
    let keep: HashSet<usize> = desired.iter().map(|w| w.as_ptr() as usize).collect();
    let mut child = container.first_child();
    while let Some(c) = child {
        let next = c.next_sibling();
        if !keep.contains(&(c.as_ptr() as usize)) {
            container.remove(&c);
        }
        child = next;
    }
    let mut prev: Option<gtk::Widget> = None;
    for widget in desired {
        if widget.parent().is_some() {
            container.reorder_child_after(widget, prev.as_ref());
        } else {
            container.insert_child_after(widget, prev.as_ref());
        }
        prev = Some(widget.clone());
    }
}

#[cfg(test)]
mod page_error_tests {
    use super::*;
    use fauna_conversations::address::Rail;
    use fauna_conversations::capabilities::derive_capabilities;
    use fauna_conversations::compose::ComposeState;
    use fauna_conversations::thread::{ThreadFlavor, ThreadId};
    use fauna_core::localized::LocalizedText;

    fn empty_snapshot() -> ConversationsSnapshot {
        ConversationsSnapshot {
            threads: Vec::new(),
            sort: Default::default(),
            search_query: None,
            selected_thread_id: None,
            new_thread_compose: None,
            add_participant: None,
            error: None,
            launch_floor_ms: 0,
            room_invitations: Vec::new(),
            bridges: Vec::new(),
        }
    }

    fn thread_detail_with_send_state(send_state: SendState) -> ThreadDetail {
        let rail = Rail::FaunaMls;
        let flavor = ThreadFlavor::OneToOne;
        ThreadDetail {
            thread_id: ThreadId("t-1".into()),
            rail,
            glyph: rail.glyph(),
            flavor: flavor.clone(),
            label: "test thread".into(),
            participants: Vec::new(),
            participant_displays: Vec::new(),
            capabilities: derive_capabilities(rail, flavor),
            messages: Vec::new(),
            compose: ComposeState {
                send_state,
                ..Default::default()
            },
            selected_message_id: None,
            bridge: None,
            guardian_state: None,
            room: None,
        }
    }

    /// The gap this track closes: before it, a failed
    /// `confirm_add_participant`/`remove_participant`/`rename_thread` only
    /// `tracing::warn!`d — the overlay closed and nothing appeared, a dropped
    /// command by `testing.md` point 11's definition. `snapshot.error` must
    /// now reach `error-message`, and outrank a stale compose-send failure
    /// (every producer clears `snapshot.error` on entry, so it is always the
    /// fresher of the two).
    #[test]
    fn snapshot_error_outranks_a_failed_compose_send() {
        let mut snap = empty_snapshot();
        snap.error = Some(LocalizedText::key_arg(
            "conversations.unified.error_add_participant",
            "message",
            "no key package published",
        ));
        snap.new_thread_compose = Some(ComposeState {
            send_state: SendState::failed("send failed"),
            ..Default::default()
        });

        let shown =
            page_error_text(&snap, None, false, false, 0).expect("snapshot.error must surface");
        assert!(
            shown.contains("no key package published"),
            "the backend's own reason must reach the user, got {shown:?}"
        );
        assert!(
            !shown.contains("conversations.unified"),
            "the i18n key must be resolved, not painted raw: {shown:?}"
        );
    }

    /// The standing conversations-engine-role refusal (`account-data-plane.md`
    /// § Multi-instance concurrency, W5.6 (account-data-plane.md § Workstreams)) must never be masked by an
    /// unrelated gesture's `snapshot.error` — the whole reason it is a
    /// separate manager field rather than folded into that clearable slot.
    #[test]
    fn served_elsewhere_outranks_snapshot_error_and_a_failed_send() {
        let mut snap = empty_snapshot();
        snap.error = Some(LocalizedText::key_arg(
            "conversations.unified.error_add_participant",
            "message",
            "no key package published",
        ));
        snap.new_thread_compose = Some(ComposeState {
            send_state: SendState::failed("send failed"),
            ..Default::default()
        });

        let shown = page_error_text(&snap, None, true, false, 0)
            .expect("the standing refusal must surface even with other errors stamped");
        assert_eq!(
            shown,
            crate::i18n::strings::conversations::errors::SERVED_ELSEWHERE,
            "got {shown:?}"
        );
    }

    /// The dead receive rail on `error-message`: a loop that died by panic is
    /// a standing truth the manager holds
    /// (`ConversationsManager::receive_stopped`), ranked directly under the
    /// served-elsewhere refusal and above both `snapshot.error` and a failed
    /// send. Mirrors `served_elsewhere_outranks_snapshot_error_and_a_failed_send`
    /// above, and tui's
    /// `sync_page_error_surfaces_a_stopped_receive_rail_and_no_success_fold_clears_it`.
    #[test]
    fn receive_stopped_outranks_snapshot_error_and_a_failed_send_but_not_served_elsewhere() {
        let mut snap = empty_snapshot();
        snap.error = Some(LocalizedText::key_arg(
            "conversations.unified.error_add_participant",
            "message",
            "no key package published",
        ));
        snap.new_thread_compose = Some(ComposeState {
            send_state: SendState::failed("send failed"),
            ..Default::default()
        });

        let shown = page_error_text(&snap, None, false, true, 0)
            .expect("a stopped receive rail must surface even with other errors stamped");
        assert_eq!(
            shown,
            crate::i18n::strings::conversations::errors::RECEIVE_STOPPED,
            "got {shown:?}"
        );

        let shown_under_served_elsewhere = page_error_text(&snap, None, true, true, 0)
            .expect("served_elsewhere still outranks a stopped receive rail");
        assert_eq!(
            shown_under_served_elsewhere,
            crate::i18n::strings::conversations::errors::SERVED_ELSEWHERE,
            "got {shown_under_served_elsewhere:?}"
        );
    }

    /// A newer receive loop over the same manager retires the notice — the
    /// half of tui's mirrored test that lives in `ConversationsManager`
    /// itself rather than in `page_error_text`'s pure precedence logic (no
    /// gesture clears it, but a fresh loop does).
    #[test]
    fn a_newer_receive_loop_retires_a_stopped_rails_notice() {
        let manager = ConversationsManager::new();
        let generation = manager.begin_receive_loop();
        manager.mark_receive_stopped(generation);
        assert!(
            manager.receive_stopped(),
            "sanity: the manager now reports a stopped rail"
        );

        let snap = empty_snapshot();
        let shown = page_error_text(&snap, None, false, manager.receive_stopped(), 0)
            .expect("a receive loop that died by panic must surface on error-message");
        assert_eq!(
            shown,
            crate::i18n::strings::conversations::errors::RECEIVE_STOPPED,
            "got {shown:?}"
        );

        manager.begin_receive_loop();
        assert!(
            !manager.receive_stopped(),
            "a newer receive loop over the same manager must retire the notice"
        );
        assert_eq!(
            page_error_text(&snap, None, false, manager.receive_stopped(), 0),
            None,
            "the retired notice must not keep surfacing"
        );
    }

    /// The send reason is a `LocalizedText` like the page error, so this asserts
    /// the same two mutations the page-error test does: the backend's own detail
    /// reaches the user, **and** the key went through the i18n pipeline rather
    /// than being painted raw (a verbatim-equality assertion could not tell those
    /// apart, which is why it was replaced when the reason gained its key).
    #[test]
    fn falls_back_to_the_new_thread_composes_send_failure() {
        let mut snap = empty_snapshot();
        snap.new_thread_compose = Some(ComposeState {
            send_state: SendState::failed("nest rejected fauna.email.send"),
            ..Default::default()
        });

        let shown =
            page_error_text(&snap, None, false, false, 0).expect("a failed send must surface");
        assert!(
            shown.contains("nest rejected fauna.email.send"),
            "the backend's own reason must reach the user, got {shown:?}"
        );
        assert!(
            !shown.contains("conversations.unified"),
            "the i18n key must be resolved, not painted raw: {shown:?}"
        );
    }

    #[test]
    fn falls_back_to_the_selected_threads_compose_when_no_new_thread_compose_is_active() {
        let snap = empty_snapshot();
        let detail = thread_detail_with_send_state(SendState::failed("boom"));

        let shown = page_error_text(&snap, Some(&detail), false, false, 0)
            .expect("the selected thread's failure surfaces");
        assert!(shown.contains("boom"), "got {shown:?}");
        assert!(
            !shown.contains("conversations.unified"),
            "the i18n key must be resolved, not painted raw: {shown:?}"
        );
    }

    #[test]
    fn no_error_and_no_failed_send_shows_nothing() {
        let snap = empty_snapshot();
        assert_eq!(page_error_text(&snap, None, false, false, 0), None);
    }

    /// The floor of the stack (`ui/conversations.md` § Errors & edge cases →
    /// *A fifth truth*): received mail this run that could not open under the
    /// account's key set shows only when nothing else does, mirrors tui's
    /// `sync_page_error_surfaces_skipped_unopenable_mail_below_every_other_error`.
    #[test]
    fn unopenable_mail_shows_only_when_nothing_else_present() {
        let snap = empty_snapshot();
        assert_eq!(
            page_error_text(&snap, None, false, false, 0),
            None,
            "sanity: nothing skipped shows no error"
        );

        let shown = page_error_text(&snap, None, false, false, 2)
            .expect("skipped unopenable mail must surface on error-message");
        assert!(
            shown.starts_with("2 ") && shown.contains("could not be opened"),
            "the count must be substituted through the generated formatter, not \
             painted as a raw key: {shown:?}"
        );
    }

    /// Every other truth outranks the unopenable-mail floor — it is a floor,
    /// never a mask.
    #[test]
    fn a_failed_send_outranks_unopenable_mail() {
        let mut snap = empty_snapshot();
        snap.new_thread_compose = Some(ComposeState {
            send_state: SendState::failed("send failed"),
            ..Default::default()
        });

        let shown = page_error_text(&snap, None, false, false, 2)
            .expect("a failed send must surface over the unopenable-mail floor");
        assert!(
            shown.contains("send failed"),
            "the fresher send failure must win, got {shown:?}"
        );
    }

    #[test]
    fn served_elsewhere_outranks_unopenable_mail() {
        let snap = empty_snapshot();
        let shown = page_error_text(&snap, None, true, false, 2)
            .expect("the standing refusal must surface over the unopenable-mail floor");
        assert_eq!(
            shown,
            crate::i18n::strings::conversations::errors::SERVED_ELSEWHERE,
            "got {shown:?}"
        );
    }
}

#[cfg(test)]
mod member_review_tests {
    use super::*;
    use fauna_conversations::TypedAddress;
    use fauna_core::data::{MemberReview, MemberUnattestedReason};
    use fauna_core::identity::ActorId;

    /// A client whose nest is a closed port — nothing here dials (`walk.rs`'s
    /// own fixture idiom). The receiver is handed back so every `UiSender::send`
    /// has somewhere to land.
    fn offline_client() -> (
        Rc<FaunaClient>,
        std::sync::mpsc::Receiver<crate::app::UiMessage>,
    ) {
        let (tx, rx) = crate::client::ui_channel();
        let machine = fauna_launch_machine::LaunchMachine::new(
            Arc::new(fauna_launch_machine::NullObserver),
            Arc::new(fauna_launch_machine::InMemoryPersistence::new()),
        );
        let client = Rc::new(FaunaClient::new(
            "http://127.0.0.1:1".to_string(),
            "11".repeat(32),
            tx,
            machine,
        ));
        (client, rx)
    }

    /// **A roster that lands after the pane is built marks the open thread's
    /// member, with no snapshot tick in between.** That order is not an edge
    /// case, it is a successor's first session: the window — and this pane — is
    /// built at sign-in, while the aftermath raises the roster only after
    /// its pass has run over the network. A pane that read the
    /// roster once when it was built showed no mark for that whole session.
    ///
    /// Reds when the pane stops repainting on the roster (the mark then waits
    /// for the thread's next unrelated change), and when it reads a roster of
    /// its own instead of the window's.
    #[test]
    fn a_roster_that_lands_after_the_pane_is_built_marks_the_open_threads_member() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let person = ActorId([5u8; 32]);
            let manager = ConversationsManager::new();
            let thread = manager.create_mls_group(vec![TypedAddress::Fauna {
                handle: "bob".into(),
                actor_id: person,
            }]);
            manager.select_thread(thread.clone());
            let (client, _rx) = offline_client();
            let roster = Rc::new(Roster::default());
            let pane = ConversationDetail::new(
                manager.clone(),
                Rc::clone(&client),
                client.runtime_handle(),
                Rc::clone(&roster),
            );
            let detail = manager.thread_detail(thread).expect("the group is open");
            pane.render(&manager.snapshot(), Some(&detail), "", false, false, 0);

            let root: gtk::Widget = pane.root.clone().upcast();
            assert!(
                crate::automation::find::find_in(&root, ids::THREAD_MEMBER_CHIP).is_some(),
                "the fixture must render the member's chip, or the mark below is vacuous"
            );
            assert!(
                crate::automation::find::find_in(&root, ids::THREAD_MEMBER_UNATTESTED_MARK)
                    .is_none(),
                "nobody is flagged before the roster lands"
            );

            roster.set(vec![MemberReview {
                person,
                reasons: vec![MemberUnattestedReason::CompromiseWindow],
            }]);

            assert!(
                crate::automation::find::find_in(&root, ids::THREAD_MEMBER_UNATTESTED_MARK)
                    .is_some(),
                "the roster landed after the pane was built, and the open thread's \
                 flagged member still carries no mark"
            );
        });
    }
}
