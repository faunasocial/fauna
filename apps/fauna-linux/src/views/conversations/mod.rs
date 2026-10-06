//! Conversations page orchestrator.
//!
//! Wires the list pane + detail pane (3-state stack) over a shared
//! `ConversationsManager`. Subscribes a `GtkConversationsObserver` and
//! drives a `Refresh` closure on every snapshot tick that re-reads the
//! current snapshot + thread detail and rebuilds widget contents.
//!
//! Cheap full-rebuild model mirrors Windows Phase 5c — small lists, no
//! diffing. The observer marshals onto the GTK main loop via
//! `glib::MainContext::default().spawn_local`.

mod add_participant_overlay;
pub mod compose_bar;
mod compose_decoration;
pub mod compose_toolbar;
pub mod detail;
pub mod list;
pub mod message_bubble;
// `pub` so the folders Sharing UI can reuse the picker (priority #2 — no new
// picker IDs; folders.md § Sharing reuses the conversations recipient-picker).
pub mod recipient_picker;
mod rename_overlay;
mod room_settings_overlay;
mod subject_divider;
mod thread_header;

use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use fauna_conversations::ConversationsManager;

use crate::conversations::observer;

/// Build the conversations page widget. Returns the outer `NavigationSplitView`
/// for placement into the main window's `gtk::Stack`. The page owns its own
/// observer subscription; calling `build_conversations_view` repeatedly
/// would attach multiple observers — the orchestrator is intended to be
/// called once per app session.
///
/// `member_reviews` is the window's post-succession review roster, shared with
/// the contacts badge — the one piece of this page's state it does not own.
pub fn build_conversations_view(
    manager: Arc<ConversationsManager>,
    client: std::rc::Rc<crate::client::FaunaClient>,
    rt: tokio::runtime::Handle,
    member_reviews: std::rc::Rc<crate::settings::member_review::Roster>,
) -> adw::NavigationSplitView {
    // The session's own actor — never `active_actor_id_hex()`, which can name
    // a different account than the one this process serves on a bound
    // (secondary) launch (`account-scoping.md:818-837`).
    let actor_id_hex = client.actor_id().unwrap_or_default();
    let list_pane = list::ConversationList::new(manager.clone(), actor_id_hex);
    let detail_pane = detail::ConversationDetail::new(manager.clone(), client, rt, member_reviews);

    // Wrap in NavigationSplitView (mirrors the previous layout shape).
    let list_page = adw::NavigationPage::builder()
        .title(crate::i18n::strings::conversations::list::TITLE)
        .child(&list_pane.root)
        .build();
    let detail_page = adw::NavigationPage::builder()
        .title(crate::i18n::strings::conversations::detail::TITLE)
        .child(&detail_pane.root)
        .build();

    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&list_page));
    split.set_content(Some(&detail_page));

    // Subscribe to snapshot changes. The receiver runs on the GTK main
    // loop via `spawn_wake_loop`; on every coalesced wake re-render both panes.
    let rx = observer::attach(&manager);
    {
        let manager = manager.clone();
        // Initial render so the empty state shows correctly even
        // before the first mutation.
        refresh(&manager, &list_pane, &detail_pane);
        {
            // Self-terminate when this view's window is torn down. The
            // authenticated window is rebuilt on every session change (re-login,
            // and — in e2e — every `set_state` session patch), each rebuild
            // calling `build_conversations_view` again against the *app-level*
            // `ConversationsManager` singleton. The old window is destroyed
            // (`main.rs` `v.window.destroy()`) but this `spawn_local` task is not
            // explicitly aborted, so without this guard each rebuild leaks an
            // observer + detail pane that keeps re-rendering destroyed widgets
            // forever against the shared manager — and, whenever
            // `snapshot.add_participant` is set, every leaked pane re-presents the
            // add-participant dialog (an N-way dialog storm that wedges the UI).
            // Once our widget tree has been rooted and then loses its window
            // (the rebuild destroyed it), exit: dropping `list_pane`/`detail_pane`
            // closes our now-orphaned dialog and frees the observer, leaving only
            // the live window's observer attached.
            let mut was_rooted = false;
            // A closed channel (sender dropped) shouldn't happen — the loop's
            // exit on it is the right behavior either way.
            crate::async_helper::spawn_wake_loop(rx, move || {
                if detail_pane.root.root().is_some() {
                    was_rooted = true;
                } else if was_rooted {
                    return glib::ControlFlow::Break;
                }
                refresh(&manager, &list_pane, &detail_pane);
                glib::ControlFlow::Continue
            });
        }
    }

    split
}

fn refresh(
    manager: &Arc<ConversationsManager>,
    list_pane: &list::ConversationList,
    detail_pane: &detail::ConversationDetail,
) {
    let snap = manager.snapshot();
    let active_detail = snap
        .selected_thread_id
        .as_ref()
        .and_then(|id| manager.thread_detail(id.clone()));
    list_pane.render(
        &snap.threads,
        snap.selected_thread_id.as_ref().map(|i| i.0.as_str()),
    );
    // Linux app doesn't yet thread the local user's display handle
    // through to here; pass empty so all bubbles render as peer for now.
    // Future cleanup can lift this out of session state once the
    // launch flow finishes plumbing it.
    let self_handle = "";
    detail_pane.render(
        &snap,
        active_detail.as_ref(),
        self_handle,
        manager.engine_served_elsewhere(),
        manager.receive_stopped(),
        manager.unopenable_mail_count(),
    );
}
