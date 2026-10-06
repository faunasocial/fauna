use crate::i18n::strings::{common, notifications as notif_text};
use notify_rust::{Notification, Urgency};
use std::sync::atomic::Ordering;
use std::thread;

// ---------------------------------------------------------------------------
// Replace-ID constants for notification grouping.
// Reusing the same ID causes the notification daemon to replace the previous
// notification of the same type rather than stacking new ones.
// ---------------------------------------------------------------------------
const NOTIF_ID_MESSAGE: u32 = 1001;
// `NOTIF_ID_GROUP_MESSAGE`/`NOTIF_ID_GROUP_INVITE` back the two group-specific
// notifiers below, which have no caller today — group activity currently
// surfaces only through the generic `notify_unified` dispatch, never through
// these dedicated group notifiers. Not deleted: unlike `event_form.rs`'s dead
// builders, these are complete, correctly-wired notification calls, just
// never reached from `app.rs`'s DataMessage dispatch.
#[allow(dead_code)]
const NOTIF_ID_GROUP_MESSAGE: u32 = 1002;
const NOTIF_ID_KNOCK: u32 = 1003;
#[allow(dead_code)]
const NOTIF_ID_GROUP_INVITE: u32 = 1004;
const NOTIF_ID_UNIFIED: u32 = 1007;

/// Returns `true` when notifications should be shown.
///
/// Suppresses notifications while the main window is in focus — the user is
/// already looking at the application and does not need an OS popup.
pub fn should_show_notifications() -> bool {
    !crate::tray::WINDOW_FOCUSED.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// Internal helper — add a sound hint when the setting is enabled.
// ---------------------------------------------------------------------------

/// Apply the notification sound hint if the user has sound enabled.
///
/// `notify-rust` exposes `.sound_name()` which sets the
/// `sound-name` freedesktop hint so the notification daemon plays the
/// standard sound through the desktop's audio stack.
fn maybe_add_sound(notif: &mut Notification) {
    if crate::tray::NOTIFICATION_SOUND.load(Ordering::SeqCst) {
        notif.sound_name("message-new-instant");
    }
}

// ---------------------------------------------------------------------------
// Internal helper — spawn a thread that waits for the default action click
// and raises the main window via the TRAY_RAISE flag.
// ---------------------------------------------------------------------------

/// Escape `&`, `<` and `>` in a toast body built from remote-authored text.
///
/// The freedesktop spec lets a notification daemon render body markup
/// (`<b>`, `<a href>`, `<img>`), so a contact's or stranger's text must reach
/// it as text. (The shared-Rust sanitizer already made a knock single-line
/// plain text; this is the shell's half.)
fn escape_body_markup(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn spawn_action_listener(handle: notify_rust::NotificationHandle) {
    thread::spawn(move || {
        handle.wait_for_action(|action| {
            if action == "default" || action == "__closed" {
                // Signal the GTK poll loop to raise the main window.
                crate::tray::TRAY_RAISE.store(true, Ordering::SeqCst);
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Public notification functions
// ---------------------------------------------------------------------------

/// Show a desktop notification for a new direct message.
///
/// Uses replace-ID `NOTIF_ID_MESSAGE` so rapid messages from the same
/// conversation collapse into a single notification rather than flooding
/// the notification centre.  A background thread waits for the user to
/// click the notification and raises the main window via `TRAY_RAISE`.
///
/// ⚠ **Deliberately does NOT consult [`should_show_notifications`]** (corrected
/// 2026-09-20). The when/for-whom decision for a new-message banner has exactly
/// three rules and they all live in the shared `MessageNotificationTracker`
/// (`conversations.md` § Where logic lives): seed silently, fire on new activity,
/// suppress the thread you have open. The window-focus gate was a **fourth**
/// rule, in app glue, that no goal doc ever stated and that windows — the other
/// column that built this firing — does not apply; it predates the 2026 lift of
/// the decision into shared Rust. It also falsified the ratified promise for the
/// ordinary case (app focused, reading the Feed, a DM arrives in a thread that
/// is not open): `docs/features/conversations.md` outcome 11 says a new message
/// raises a system notification while the app is running, and names the open
/// conversation as the one exception. The other notifiers below keep the gate —
/// no ratified outcome covers them, so narrowing this to the message banner is
/// the conformance fix and not a sweep.
pub fn notify_message(sender: &str, body_preview: &str) {
    let mut notif = Notification::new();
    notif
        .summary(&notif_text::message_from(sender))
        .body(&escape_body_markup(body_preview))
        .icon("fauna")
        .appname("Fauna")
        .urgency(Urgency::Normal)
        .id(NOTIF_ID_MESSAGE)
        .action("default", common::OPEN)
        .timeout(5000);
    maybe_add_sound(&mut notif);

    if let Ok(handle) = notif.show() {
        spawn_action_listener(handle);
    }
}

/// Show a desktop notification for a new group message.
///
/// Uses replace-ID `NOTIF_ID_GROUP_MESSAGE` so rapid group messages collapse.
#[allow(dead_code)]
pub fn notify_group_message(group_name: &str, sender: &str, body_preview: &str) {
    if !should_show_notifications() {
        return;
    }

    let mut notif = Notification::new();
    notif
        .summary(&notif_text::group_message(sender, group_name))
        .body(&escape_body_markup(body_preview))
        .icon("fauna")
        .appname("Fauna")
        .urgency(Urgency::Normal)
        .id(NOTIF_ID_GROUP_MESSAGE)
        .action("default", common::OPEN)
        .timeout(5000);
    maybe_add_sound(&mut notif);

    if let Ok(handle) = notif.show() {
        spawn_action_listener(handle);
    }
}

/// Show a desktop notification for a new knock (contact request).
///
/// `body` is the shared decision's text (`crate::i18n::knock_push_text`) — the
/// knock row's own sentence, so the toast says what the row says.
pub fn notify_knock(body: &str) {
    if !should_show_notifications() {
        return;
    }

    let mut notif = Notification::new();
    notif
        .summary(notif_text::KNOCK_TITLE)
        .body(&escape_body_markup(body))
        .icon("fauna")
        .appname("Fauna")
        .urgency(Urgency::Normal)
        .id(NOTIF_ID_KNOCK)
        .action("default", common::OPEN)
        .timeout(5000);
    maybe_add_sound(&mut notif);

    if let Ok(handle) = notif.show() {
        spawn_action_listener(handle);
    }
}

/// Show a desktop notification when a file has been successfully uploaded.
///
/// Uses replace-ID `1005` so rapid uploads collapse into a single
/// notification rather than flooding the notification centre.
/// Does not add a sound hint — sync completions are low-priority events.
pub fn notify_sync_complete(filename: &str) {
    if !should_show_notifications() {
        return;
    }

    let _ = Notification::new()
        .summary(notif_text::SYNC_COMPLETE_TITLE)
        .body(&notif_text::sync_complete_body(filename))
        .icon("fauna")
        .appname("Fauna")
        .urgency(Urgency::Low)
        .id(1005)
        .timeout(3000)
        .show();
}

/// Show a persistent desktop notification for an event starting soon.
///
/// Uses replace-ID `1006` so repeated polls for the same event replace the
/// previous reminder rather than stacking.
pub fn notify_event_reminder(event_name: &str, minutes: u64) {
    let _ = Notification::new()
        .summary(notif_text::EVENT_REMINDER_TITLE)
        .body(&notif_text::event_reminder_body(
            event_name,
            &minutes.to_string(),
        ))
        .icon("fauna")
        .appname("Fauna")
        .urgency(Urgency::Normal)
        .id(1006)
        .timeout(0) // persistent until dismissed
        .show();
}

/// Show a desktop notification for a group invite with Accept/Decline actions.
///
/// Group invites use `Urgency::Critical` and persist until dismissed.
///
/// Note: `notify_rust`'s `NotificationHandle::wait_for_action()` blocks the
/// calling thread, so action responses cannot be processed on the GTK main
/// thread.  Spawning a background thread to call `wait_for_action` is not
/// feasible with `FaunaClient` (`Rc`-based, not `Send`) so only the default
/// "Open" action raises the window; users accept/decline inside the app.
/// Migrating `FaunaClient` to `Arc` would allow full in-notification handling.
#[allow(dead_code)]
pub fn notify_group_invite(from: &str, group_name: &str) {
    if !should_show_notifications() {
        return;
    }

    let mut notif = Notification::new();
    notif
        .summary(notif_text::GROUP_INVITE_TITLE)
        .body(&notif_text::group_invite_body(from, group_name))
        .icon("fauna")
        .appname("Fauna")
        .urgency(Urgency::Critical)
        .id(NOTIF_ID_GROUP_INVITE)
        .action("default", &format!("{} Fauna", common::OPEN))
        .timeout(0); // persistent until user acts
    maybe_add_sound(&mut notif);

    if let Ok(handle) = notif.show() {
        spawn_action_listener(handle);
    }
}

/// Show a desktop notification for unified activity (likes, replies, follows,
/// reposts, mentions, quotes — across fauna, bluesky, nostr, AP).
///
/// `notif_type` is the typed action (`like` / `reply` / `follow` / etc.) used
/// to pick a title; `summary` is the localised body text the nest already
/// produced (e.g. "@alice liked your post").
///
/// Uses replace-ID `NOTIF_ID_UNIFIED` so rapid notifications collapse into a
/// single desktop popup.
pub fn notify_unified(notif_type: &fauna_protocol::notifications::NotifType, summary: &str) {
    if !should_show_notifications() {
        return;
    }

    let title = match notif_type {
        fauna_protocol::notifications::NotifType::Like => notif_text::TYPE_LIKE,
        fauna_protocol::notifications::NotifType::Reply => common::REPLY,
        fauna_protocol::notifications::NotifType::Follow => common::FOLLOW,
        fauna_protocol::notifications::NotifType::Repost => notif_text::TYPE_REPOST,
        fauna_protocol::notifications::NotifType::Mention => notif_text::TYPE_MENTION,
        fauna_protocol::notifications::NotifType::Quote => notif_text::TYPE_QUOTE,
        _ => notif_text::TYPE_DEFAULT,
    };

    let mut notif = Notification::new();
    notif
        .summary(title)
        .body(summary)
        .icon("fauna")
        .appname("Fauna")
        .urgency(Urgency::Normal)
        .id(NOTIF_ID_UNIFIED)
        .action("default", common::OPEN)
        .timeout(5000);
    maybe_add_sound(&mut notif);

    if let Ok(handle) = notif.show() {
        spawn_action_listener(handle);
    }
}

#[cfg(test)]
mod escape_tests {
    use super::escape_body_markup;

    #[test]
    fn body_markup_is_escaped_ampersand_first() {
        assert_eq!(
            escape_body_markup("<a href=\"x\">go</a> & <b>"),
            "&lt;a href=\"x\"&gt;go&lt;/a&gt; &amp; &lt;b&gt;"
        );
    }
}
