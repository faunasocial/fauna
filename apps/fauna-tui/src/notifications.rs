//! The notifications page — M5 slice 1 (`behavior/notifications.md`; ui.yaml
//! `notifications`).
//!
//! Transport-only, the contacts shape (`crate::contacts`): no shared manager
//! exists for notifications — clients consume the typed
//! `fauna_client_notifications::NotificationsClient` and render the wire
//! replies directly (`behavior/notifications.md` § State & data shape, the
//! landed decision) — so the rows live on `App` and refetch at login + on
//! every navigation to the tab (the `App::apply` nav-edge hook).
//!
//! Rows DEEP-LINK since 2026-09-21, tui leading all 7 apps: the shared
//! `notification_destination` decides where each row goes and
//! [`open_notification`] routes it into the destination page's own gesture
//! (`behavior/notifications.md` § Deep-link destinations). A row the router
//! gives no destination stays a plain label — an inert row must look inert.
//!
//! What remains of that doc's declared-draft remainder is the typed-enum
//! evolution: the type icon is still a client-mapped glyph off the plain
//! `notif_type` string. The unread badge reads the dedicated `fauna.notifications.count`
//! reply (§ Persistence), not a client-side count over the loaded page —
//! the loaded page is one page (default 25) of a possibly-longer history.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_notifications::{
    NotificationDestination, NotificationText, NotificationsClient, notification_destination,
    notification_muted, notification_text,
    notifications::{NotifItem, NotifType, is_security_notice_retained},
};
use fauna_i18n::strings::common as t;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, UiMessage};
use crate::element::{Element, Gesture};
use crate::pages::Page;

/// Page state: the loaded page of rows + the nest-reported unread count.
#[derive(Default)]
pub struct NotificationsState {
    /// The live client, `None` pre-login (elements render an empty page).
    nest: Option<Arc<NestClient>>,
    pub items: Vec<NotifItem>,
    pub unread: i64,
    /// Rows the user revealed past their muted-word collapse — per-row and
    /// session-local, the mute stays (`behavior/notifications.md` § Muted words
    /// rule 3; the `crate::conversations` `revealed_muted` shape).
    pub revealed_muted: std::collections::HashSet<i64>,
}

/// Build the page state at the post-auth hook and start the initial fetch
/// (hydrate on login; navigating to the tab refetches — the contacts pattern).
pub fn init(
    nest: Arc<NestClient>,
    tx: &UnboundedSender<UiMessage>,
    session_generation: u64,
) -> NotificationsState {
    let state = NotificationsState {
        nest: Some(nest),
        ..NotificationsState::default()
    };
    spawn_refresh(&state, tx, session_generation);
    state
}

/// Fire-and-forget list + count refetch; the result lands through the channel.
/// Used at login and on nav-to-tab — the agent's click path awaits its op
/// inline instead.
/// The refetch entering this tab implies — the page's leg of the one nav-edge
/// hook (`crate::app::on_nav_enter`). Returns the op; the caller runs it.
pub fn nav_enter_op(state: &NotificationsState) -> Option<Op> {
    Some(Op::Refresh {
        nest: state.nest.clone()?,
    })
}

/// Fire-and-forget the refetch — the **post-auth** path only (no driver ack to
/// honour). The nav edge goes through [`nav_enter_op`] so it can be awaited.
pub fn spawn_refresh(
    state: &NotificationsState,
    tx: &UnboundedSender<UiMessage>,
    session_generation: u64,
) {
    crate::app::spawn_nav_refresh!(nav_enter_op(state), tx, session_generation, Notifications);
}

/// The page's own gestures (`behavior/notifications.md` § User actions). Row
/// activation is NOT here — it navigates to another page, so it rides
/// [`crate::element::Gesture::OpenNotification`] and
/// [`open_notification`], the `OpenSearchResult` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MarkAllRead,
    /// `notification-dismiss[i]` — delete the row of this id (§ Retention
    /// rule 2).
    Dismiss(i64),
    /// `notification-clear` — delete every row up to the newest one shown.
    ClearAll,
    /// `notification-muted-reveal-button[i]` — paint the collapsed row of this
    /// id in full for the session (§ Muted words rule 3).
    RevealMuted(i64),
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`).
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // `notifications_mark_read(None)` — the whole-list form.
            Action::MarkAllRead => Some("fauna.notifications.mark_read"),
            Action::Dismiss(_) => Some("fauna.notifications.dismiss"),
            Action::ClearAll => Some("fauna.notifications.clear"),
            Action::RevealMuted(_) => None,
        }
    }
}

/// Local half of a gesture → its network half (the feed/conversations split:
/// the agent awaits the op, the keyboard spawns it).
pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    if let Action::RevealMuted(id) = action {
        // A render decision over rows already loaded: one set insert, no write.
        app.notifications.revealed_muted.insert(id);
        return None;
    }
    let st = &app.notifications;
    match action {
        Action::MarkAllRead => Some(Op::MarkAllRead {
            nest: st.nest.clone()?,
        }),
        Action::Dismiss(id) => Some(Op::Dismiss {
            nest: st.nest.clone()?,
            id,
        }),
        Action::ClearAll => Some(Op::Clear {
            nest: st.nest.clone()?,
            up_to: clear_up_to(&st.items)?,
        }),
        Action::RevealMuted(_) => None,
    }
}

/// The `up_to` a clear-all sends: the newest `created_at` the page has shown,
/// so a row that arrived after the user looked survives (§ Retention rule 2).
/// Every loaded row counts — a muted row collapsed in the paint is still on
/// the page, and clear reaches it exactly as an open one (§ Muted words rule
/// 4). `None` on an empty page, where the button is disabled: a clear with no
/// bound would delete rows the user never saw.
fn clear_up_to(items: &[NotifItem]) -> Option<i64> {
    items.iter().map(|item| item.created_at).max()
}

/// The network half. Every variant refetches after its mutation — the reply is
/// a bare count echo, so the fresh list + count ARE the observable effect
/// (the contacts refetch-after-mutate convention).
pub enum Op {
    Refresh { nest: Arc<NestClient> },
    MarkAllRead { nest: Arc<NestClient> },
    Dismiss { nest: Arc<NestClient>, id: i64 },
    Clear { nest: Arc<NestClient>, up_to: i64 },
}

/// What an [`Op`] resolved to; folded back into the page by [`apply_outcome`].
pub enum Outcome {
    Loaded { items: Vec<NotifItem>, unread: i64 },
    Failed(String),
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Loaded { items, unread } => f
                .debug_struct("Loaded")
                .field("items", &items.len())
                .field("unread", unread)
                .finish(),
            Outcome::Failed(e) => f.debug_tuple("Failed").field(e).finish(),
        }
    }
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Refresh { nest } => fetch(nest).await,
            Op::MarkAllRead { nest } => {
                let client = NotificationsClient::new(Arc::clone(&nest));
                if let Err(e) = client.notifications_mark_read(None).await {
                    return Outcome::Failed(format!("mark read: {e}"));
                }
                fetch(nest).await
            }
            // A `fauna.notifications.retained` refusal (a security notice the
            // greyed button let through at the window's edge) is an error like
            // any other here: it lands on `error-message`, never a silent no-op.
            Op::Dismiss { nest, id } => {
                let client = NotificationsClient::new(Arc::clone(&nest));
                if let Err(e) = client.notifications_dismiss(id).await {
                    return Outcome::Failed(format!("dismiss: {e}"));
                }
                fetch(nest).await
            }
            Op::Clear { nest, up_to } => {
                let client = NotificationsClient::new(Arc::clone(&nest));
                if let Err(e) = client.notifications_clear(Some(up_to)).await {
                    return Outcome::Failed(format!("clear: {e}"));
                }
                fetch(nest).await
            }
        }
    }
}

/// One page of rows (nest default limit) + the authoritative unread count.
async fn fetch(nest: Arc<NestClient>) -> Outcome {
    let client = NotificationsClient::new(nest);
    let list = match client.notifications_list(None, None).await {
        Ok(r) => r,
        Err(e) => return Outcome::Failed(format!("load notifications: {e}")),
    };
    let count = match client.notifications_count().await {
        Ok(r) => r,
        Err(e) => return Outcome::Failed(format!("unread count: {e}")),
    };
    Outcome::Loaded {
        items: list.notifications,
        unread: count.count,
    }
}

/// Fold an [`Outcome`] back into the page. A failure lands on the page's
/// canonical `error-message`; a load clears it (the page shows fresh truth).
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Loaded { items, unread } => {
            app.notifications.items = items;
            app.notifications.unread = unread;
            app.errors.remove(&Page::Notifications);
        }
        Outcome::Failed(message) => {
            app.errors.insert(Page::Notifications, message);
        }
    }
}

/// Activate `notification-item[i]` — route the row's typed
/// [`NotificationDestination`] into the destination page's own gesture,
/// exactly as a direct click there would (`behavior/notifications.md`
/// § Deep-link destinations; the `crate::search::open_result` shape):
///
/// - [`NotificationDestination::Post`] → `feed::Action::OpenPostDetail`. The
///   ids match — a native `like` row's `content_id` **is** the liked post's id
///   — and that action already handles the DEEP-LINK case, fetching a post the
///   timeline never loaded and unsealing it if gated, which is exactly what a
///   notification tap needs.
/// - [`NotificationDestination::Knock`] → `contacts::Action::ShowPeople`. The
///   pending knocks (`knock-request-item`) live in the Contacts page's People
///   segment, and that action both selects the segment — the sticky segment may
///   be the Address Book — and refetches, so the knock is on screen and fresh.
///   The `sender_id` rides the target for an app that can highlight the
///   matching row; tui lists knocks flat, so it opens the page.
/// - [`NotificationDestination::Family`] → the Family page plus its nav-edge
///   refresh, the one `family::nav_enter_op` the sidebar route runs. No action
///   selects a row there: all three routed doorbells land on the same pending
///   queue / readout block, which the refresh repaints.
pub fn open_notification(
    app: &mut App,
    dest: NotificationDestination,
) -> Option<crate::app::PageOp> {
    use crate::app::PageOp;

    match dest {
        NotificationDestination::Post { post_id } => {
            app.page = Page::Feed;
            crate::feed::apply_local(app, crate::feed::Action::OpenPostDetail(post_id))
                .map(PageOp::Feed)
        }
        NotificationDestination::Knock { .. } => {
            app.page = Page::Contacts;
            crate::contacts::apply_local(app, crate::contacts::Action::ShowPeople)
                .map(PageOp::Contacts)
        }
        NotificationDestination::Family => {
            app.page = Page::Family;
            crate::family::nav_enter_op(&app.family).map(PageOp::Family)
        }
        // Off-app: a bridged post's page on its network's own website, handed
        // to the OS default browser through the one opener every external
        // link shares (`crate::os_open`). No page change and no op — the app
        // stays where it is, exactly as the wizard's provider links and the
        // media handoff leave it. The URL is shared Rust's (a fixed origin +
        // validated segments), so it is opened as-is.
        NotificationDestination::External { url } => {
            crate::os_open::open(&url);
            None
        }
    }
}

/// The destination [`open_notification`] can act on **for this app, right
/// now** — `None` paints an inert row.
///
/// Exhaustive on purpose (no `_` arm): adding a variant to the shared enum
/// without deciding here whether tui can act on it yet would silently paint a
/// dead button (search's `navigable` rule).
///
/// `Family` additionally depends on session state, not just the variant: the
/// `family-tab` is gated on [`App::has_family`], so a doorbell that outlived
/// its family link — a graduated ward's, say — would otherwise offer a
/// control that navigates to a page with no sidebar row.
fn navigable(app: &App, item: &NotifItem) -> Option<NotificationDestination> {
    match notification_destination(item)? {
        dest @ NotificationDestination::Post { .. } => Some(dest),
        dest @ NotificationDestination::Knock { .. } => Some(dest),
        NotificationDestination::Family => {
            app.has_family.then_some(NotificationDestination::Family)
        }
        // Always actionable: the OS opener is fire-and-forget and silent on a
        // headless box, which is the same posture every other external link
        // in this app takes (`os_open`'s contract).
        dest @ NotificationDestination::External { .. } => Some(dest),
    }
}

/// The glyph for a notification type — app glue over the shared typed
/// `NotifType` (`behavior/notifications.md` § Where logic lives: shared Rust
/// owns the type, the app picks the icon, because icons are platform-native —
/// a terminal's native icon is a glyph). A type with no glyph of its own, and
/// one this build does not name, paint the neutral dot.
fn type_glyph(notif_type: &NotifType) -> &'static str {
    match notif_type {
        NotifType::Like => "♥",
        NotifType::Reply => "↩",
        NotifType::Repost => "⇄",
        NotifType::Quote => "❝",
        NotifType::Follow => "＋",
        NotifType::Mention => "＠",
        NotifType::Tip => "⚡",
        _ => "•",
    }
}

/// A row's relative timestamp — the tui-wide formatter (`crate::format`);
/// `created_at` is epoch-**micro**seconds on the wire.
fn format_epoch_us(us: i64) -> String {
    crate::format::format_epoch_us(us)
}

/// The ordered ui.yaml element list (page `notifications` + the
/// `notification-row` component: `notification-type-icon`, `notification-item`
/// and `notification-dismiss`, all FLAT indexed ids — one per row in
/// registration order, like the conversations bubble children).
pub fn elements(app: &App) -> Vec<Element> {
    let st = &app.notifications;
    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX));
    let mut out = vec![
        Element::label(ids::PAGE_HEADING, t::NOTIFICATIONS),
        Element::label(
            ids::NOTIFICATION_COUNT_BADGE,
            t::unread_count(&st.unread.to_string()),
        ),
        Element::gesture_button(
            ids::NOTIFICATION_MARK_READ,
            t::MARK_ALL_READ,
            true,
            Gesture::Notifications(Action::MarkAllRead),
        ),
        Element::gesture_button(
            ids::NOTIFICATION_CLEAR,
            fauna_i18n::strings::notifications::CLEAR_ALL,
            !st.items.is_empty(),
            Gesture::Notifications(Action::ClearAll),
        ),
    ];
    for item in &st.items {
        out.push(Element::label(
            ids::NOTIFICATION_TYPE_ICON,
            type_glyph(&item.notif_type),
        ));
        // Which text — the localized body, the English summary, or the default
        // — is the shared decision (`behavior/notifications.md` § Localized
        // body); resolving the key is this app's half.
        let summary = match notification_text(item) {
            NotificationText::Localized(text) => text.resolve(fauna_i18n::strings::lookup),
            NotificationText::Verbatim(text) => text,
        };
        let when = format_epoch_us(item.created_at);
        let stamped = |text: &str| {
            if when.is_empty() {
                text.to_string()
            } else {
                format!("{text}  ·  {when}")
            }
        };
        // The muted-word collapse (§ Muted words): the shared decision over the
        // row's relayed text, against the one in-memory list the conversation
        // collapse reads too. Painted like the conversation's collapsed bubble —
        // placeholder and timestamp, its reveal, no tap target — and the
        // dismiss below stays (rule 4: the mute is a render verb, never a delete
        // guard).
        if !st.revealed_muted.contains(&item.id)
            && notification_muted(app.settings.muted_words.keywords(), item)
        {
            out.push(Element::label(
                ids::NOTIFICATION_MUTED,
                stamped(fauna_i18n::strings::muted_words::COLLAPSED),
            ));
            out.push(Element::gesture_button(
                ids::NOTIFICATION_MUTED_REVEAL_BUTTON,
                fauna_i18n::strings::muted_words::REVEAL,
                true,
                Gesture::Notifications(Action::RevealMuted(item.id)),
            ));
        } else {
            let text = stamped(&summary);
            // Only render a control for a row this app can actually act on
            // (`behavior/notifications.md` § Don't do these — a clickable row that
            // silently does nothing reads as a broken button, not an honest "this
            // one goes nowhere"). Which rows those are is the SHARED decision;
            // `navigable` only subtracts what tui cannot reach right now.
            out.push(match navigable(app, item) {
                Some(dest) => Element::gesture_button(
                    ids::NOTIFICATION_ITEM,
                    text,
                    true,
                    Gesture::OpenNotification(dest),
                ),
                None => Element::label(ids::NOTIFICATION_ITEM, text),
            });
        }
        // Every row carries its dismiss — a muted row's collapsed paint keeps
        // it too (§ Muted words rule 4: the mute is a render verb, never a
        // delete guard). Greyed on a security notice inside its window by the
        // shared predicate; the nest's refusal stays the authority.
        out.push(Element::gesture_button(
            ids::NOTIFICATION_DISMISS,
            fauna_i18n::strings::notifications::DISMISS,
            !is_security_notice_retained(&item.notif_type, item.created_at, now_us),
            Gesture::Notifications(Action::Dismiss(item.id)),
        ));
    }
    out
}

/// ui.yaml's declared `notifications.state_fields`
/// (`data.notifications.unread_count`).
pub fn state_json(state: &NotificationsState) -> serde_json::Value {
    serde_json::json!({ "unread_count": state.unread })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(notif_type: &str, summary: &str, is_read: bool) -> NotifItem {
        NotifItem {
            id: 1,
            notif_type: notif_type.into(),
            source: "fauna".into(),
            sender_id: None,
            content_id: None,
            subject_uri: None,
            summary: summary.into(),
            is_read,
            ..Default::default()
        }
    }

    /// ui.yaml `notifications`: heading, badge, mark-read, clear; then one
    /// (type-icon, item, dismiss) triple per row in registration order — flat
    /// indexed ids.
    #[test]
    fn the_page_paints_exactly_the_ui_yaml_elements() {
        let mut app = crate::app::tests::test_app();
        let els = elements(&app);
        let ids: Vec<String> = els.iter().map(|e| e.id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                "page-heading",
                "notification-count-badge",
                "notification-mark-read",
                "notification-clear",
            ],
            "the empty page paints the four chrome elements and no rows"
        );
        assert!(
            !els[3].enabled,
            "clear-all on an empty page has no shown row to bound it"
        );

        app.notifications.items = vec![item("like", "A liked your post", false)];
        app.notifications.unread = 1;
        let els = elements(&app);
        let ids: Vec<String> = els.iter().map(|e| e.id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                "page-heading",
                "notification-count-badge",
                "notification-mark-read",
                "notification-clear",
                "notification-type-icon",
                "notification-item",
                "notification-dismiss",
            ],
        );
        assert!(els[1].text.contains('1'), "badge shows the nest count");
        assert!(els[3].enabled, "clear-all is live once a row is shown");
        assert_eq!(els[4].text, "♥");
        assert!(els[5].text.starts_with("A liked your post"));
        assert_eq!(els[6].text, fauna_i18n::strings::notifications::DISMISS);
    }

    // ── Muted words (`behavior/notifications.md` § Muted words) ────────────

    fn knock(id: i64, message: &str) -> NotifItem {
        let mut row = item("knock", "a knock", false);
        row.id = id;
        row.body = Some(
            fauna_protocol::LocalizedText::new("notifications.row_knock")
                .with_arg("sender", "abcd1234")
                .with_arg("message", message),
        );
        row
    }

    fn muting(app: &mut crate::app::App, word: &str) {
        app.settings.muted_words.snapshot.keywords =
            vec![fauna_core::data::MutedKeyword::new(word)];
    }

    /// A row whose relayed text carries a Hide word paints the placeholder, its
    /// type icon, the reveal, the timestamp and its dismiss — never the sentence
    /// and never the row's tap target; the other rows paint as before.
    #[test]
    fn a_muted_row_collapses_behind_its_reveal() {
        use crate::element::Role;
        let mut app = crate::app::tests::test_app();
        muting(&mut app, "spoiler");
        app.notifications.items = vec![knock(7, "a SPOILER inside"), knock(8, "hello")];
        let els = elements(&app);
        let ids: Vec<&str> = els.iter().skip(4).map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "notification-type-icon",
                "notification-muted",
                "notification-muted-reveal-button",
                "notification-dismiss",
                "notification-type-icon",
                "notification-item",
                "notification-dismiss",
            ]
        );
        assert!(!els.iter().any(|e| e.text.contains("SPOILER")));
        let reveal = els
            .iter()
            .find(|e| e.id == ids::NOTIFICATION_MUTED_REVEAL_BUTTON)
            .unwrap();
        assert!(matches!(
            reveal.role,
            Role::Button(Gesture::Notifications(Action::RevealMuted(7)))
        ));
        let dismiss = els
            .iter()
            .find(|e| e.id == ids::NOTIFICATION_DISMISS)
            .unwrap();
        assert!(matches!(
            dismiss.role,
            Role::Button(Gesture::Notifications(Action::Dismiss(7)))
        ));
    }

    /// The reveal is one session-local set insert — no wire call — after which
    /// the row paints in full.
    #[test]
    fn the_reveal_paints_the_row_in_full() {
        let mut app = crate::app::tests::test_app();
        muting(&mut app, "spoiler");
        app.notifications.items = vec![knock(7, "a spoiler inside")];
        assert_eq!(Action::RevealMuted(7).wire_kind(), None);
        assert!(apply_local(&mut app, Action::RevealMuted(7)).is_none());
        let els = elements(&app);
        assert!(!els.iter().any(|e| e.id == ids::NOTIFICATION_MUTED));
        assert!(
            els.iter()
                .any(|e| e.id == ids::NOTIFICATION_ITEM && e.text.contains("a spoiler inside"))
        );
    }

    /// A security notice paints in full whatever its args carry (rule 5).
    #[test]
    fn a_security_notice_is_never_collapsed() {
        let mut app = crate::app::tests::test_app();
        muting(&mut app, "203.0.113");
        let mut notice = item("security.notice", "sign-in from 203.0.113.7", false);
        notice.body = Some(
            fauna_protocol::LocalizedText::new("notifications.row_security_new_token")
                .with_arg("ip", "203.0.113.7"),
        );
        app.notifications.items = vec![notice];
        let els = elements(&app);
        assert!(!els.iter().any(|e| e.id == ids::NOTIFICATION_MUTED));
        assert!(
            els.iter()
                .any(|e| e.id == ids::NOTIFICATION_ITEM && e.text.contains("203.0.113.7"))
        );
    }

    // ── The user's two deletes (`behavior/notifications.md` § Retention
    //    rule 2) ────────────────────────────────────────────────────────────

    fn now_us() -> i64 {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros(),
        )
        .unwrap()
    }

    /// Every row — whatever its type, read state or navigability — carries its
    /// own dismiss, bound to its own id. The collapsed paint of a muted row,
    /// when it lands, must keep this: § Muted words rule 4 says dismiss and
    /// clear reach a collapsed row exactly as an open one.
    #[test]
    fn every_row_carries_a_dismiss_bound_to_its_own_id() {
        use crate::element::Role;
        let mut app = crate::app::tests::test_app();
        let mut rows = vec![
            item("like", "a", false),
            item("reply", "b", true),
            item("knock", "c", false),
            item("test", "", false),
        ];
        for (n, row) in rows.iter_mut().enumerate() {
            row.id = 10 + n as i64;
        }
        app.notifications.items = rows;
        let painted = elements(&app);
        let dismiss: Vec<&Element> = painted
            .iter()
            .filter(|e| e.id == ids::NOTIFICATION_DISMISS)
            .collect();
        assert_eq!(
            dismiss.len(),
            painted
                .iter()
                .filter(|e| e.id == ids::NOTIFICATION_ITEM)
                .count()
        );
        for (n, el) in dismiss.iter().enumerate() {
            assert!(el.enabled);
            assert!(
                matches!(el.role, Role::Button(Gesture::Notifications(Action::Dismiss(id))) if id == 10 + n as i64),
                "row {n}'s dismiss must delete row {n}, got {:?}",
                el.role
            );
        }
    }

    /// The security-notice window greys the dismiss through the SHARED
    /// predicate: a notice inside its 14 days is greyed, one past it is the
    /// user's to remove, and any other type is never greyed however young.
    #[test]
    fn the_dismiss_is_greyed_only_on_a_security_notice_inside_its_window() {
        let mut app = crate::app::tests::test_app();
        let hour = 3_600_000_000;
        let mut young_notice = item("security.notice", "new sign-in", false);
        young_notice.created_at = now_us() - hour;
        let mut old_notice = item("security.notice", "old sign-in", false);
        old_notice.created_at = now_us() - 15 * 24 * hour;
        let mut young_like = item("like", "liked", false);
        young_like.created_at = now_us() - hour;
        app.notifications.items = vec![young_notice, old_notice, young_like];
        let enabled: Vec<bool> = elements(&app)
            .iter()
            .filter(|e| e.id == ids::NOTIFICATION_DISMISS)
            .map(|e| e.enabled)
            .collect();
        assert_eq!(enabled, vec![false, true, true]);
    }

    /// Clear-all is bounded by the newest row the page has shown — every
    /// loaded row, so a muted (collapsed) row is swept like any other — and
    /// sends nothing from an empty page.
    #[test]
    fn clear_all_is_bounded_by_the_newest_shown_row() {
        assert_eq!(clear_up_to(&[]), None);
        let mut rows = vec![item("like", "a", false), item("reply", "b", false)];
        rows[0].created_at = 500;
        rows[1].created_at = 900;
        assert_eq!(clear_up_to(&rows), Some(900));
    }

    /// The offline gate reads each gesture's wire kind — the two deletes name
    /// the two kinds the nest registers.
    #[test]
    fn the_deletes_issue_their_own_wire_kinds() {
        assert_eq!(
            Action::Dismiss(3).wire_kind(),
            Some("fauna.notifications.dismiss")
        );
        assert_eq!(
            Action::ClearAll.wire_kind(),
            Some("fauna.notifications.clear")
        );
    }

    /// The row paints the catalog sentence for its `body`, not the nest's
    /// English `summary` — the two differ here on purpose, since identical text
    /// could not tell which one was painted.
    #[test]
    fn a_row_with_a_known_body_paints_the_catalog_sentence_not_the_summary() {
        let mut app = crate::app::tests::test_app();
        let mut row = item("like", "ENGLISH FALLBACK", false);
        row.body = Some(
            fauna_protocol::LocalizedText::new("notifications.row_like")
                .with_arg("sender", "alice"),
        );
        app.notifications.items = vec![row];
        let els = elements(&app);
        assert!(
            els[5]
                .text
                .starts_with(&fauna_i18n::strings::notifications::row_like("alice")),
            "painted {:?}",
            els[5].text
        );
    }

    /// A key minted by a newer nest is never painted raw: the summary is.
    #[test]
    fn a_row_with_a_body_key_this_build_lacks_paints_its_summary() {
        let mut app = crate::app::tests::test_app();
        let mut row = item("like", "ENGLISH FALLBACK", false);
        row.body = Some(fauna_protocol::LocalizedText::new(
            "notifications.row_not_minted_yet",
        ));
        app.notifications.items = vec![row];
        assert!(elements(&app)[5].text.starts_with("ENGLISH FALLBACK"));
    }

    /// An empty server summary falls back to the shared default body — the
    /// linux row's exact fallback (`views/notifications.rs`).
    #[test]
    fn empty_summary_falls_back_to_the_shared_default() {
        let mut app = crate::app::tests::test_app();
        app.notifications.items = vec![item("reply", "", true)];
        let els = elements(&app);
        assert!(
            els[5]
                .text
                .starts_with(fauna_i18n::strings::notifications::DEFAULT_BODY)
        );
    }

    /// `data.notifications.unread_count` is ui.yaml's declared state field —
    /// a contract, not decoration.
    #[test]
    fn state_json_carries_the_declared_unread_count() {
        let state = NotificationsState {
            unread: 7,
            ..NotificationsState::default()
        };
        assert_eq!(state_json(&state)["unread_count"], 7);
    }

    // ── Deep-link destinations (`behavior/notifications.md` § Deep-link
    //    destinations) ──────────────────────────────────────────────────────

    /// The `search.rs` twin: a manager-backed app whose feed op can resolve a
    /// deep-linked post, sitting on the Notifications page.
    fn app_ready_to_navigate() -> App {
        let mut app = crate::app::tests::authed_app();
        app.feed.manager = Some(std::sync::Arc::new(crate::feed::CliFeedManager::new(
            crate::app::tests::test_session().client,
            [7u8; 32],
        )));
        app.page = Page::Notifications;
        app
    }

    fn like_row(post_id: &str) -> NotifItem {
        let mut row = item("like", "someone liked your post", false);
        row.sender_id = Some("bb".repeat(32));
        row.content_id = Some(post_id.into());
        row
    }

    /// A navigable row paints as a **control**; an inert one stays a label, so
    /// a click never silently does nothing (§ Don't do these — "a clickable
    /// row that does nothing reads as a broken button").
    #[test]
    fn only_navigable_rows_paint_as_gesture_buttons() {
        use crate::element::Role;
        let mut app = app_ready_to_navigate();
        app.notifications.items = vec![
            like_row("aa".repeat(32).as_str()),
            // No destination on any app: the whole detail is in the row.
            item("security.notice", "a new sign-in on your account", false),
        ];
        let painted = elements(&app);
        let roles: Vec<&Role> = painted
            .iter()
            .filter(|e| e.id == ids::NOTIFICATION_ITEM)
            .map(|e| &e.role)
            .collect();
        assert_eq!(roles.len(), 2, "one row element per notification");
        assert!(
            matches!(roles[0], Role::Button(_)),
            "a like row is navigable and must paint as a control"
        );
        assert!(
            matches!(roles[1], Role::Label),
            "a security notice has no destination and must stay inert"
        );
    }

    /// The shared router decides, not this app: a **bridged** like never
    /// routes on its `content_id` (a dedup token) — it opens the post it is
    /// about OFF-APP, on bsky.app, from `subject_uri` (user-ruled 2026-09-25),
    /// and without a subject it stays inert. This is the tui-side witness that
    /// the page consults `notification_destination` rather than matching
    /// `notif_type` itself, and that the gesture the row carries is the
    /// `External` arm with shared Rust's URL, not the raw AT-URI.
    ///
    /// The gesture is asserted on the painted element, never dispatched:
    /// `open_notification`'s `External` arm hands the URL to `os_open`, which
    /// would start a real browser on whatever box runs `cargo test`. The
    /// spawn itself is `os_open`'s own unit-tested contract.
    #[test]
    fn a_bridged_like_row_opens_its_subject_post_off_app_or_stays_inert() {
        use crate::element::Role;
        let mut app = app_ready_to_navigate();
        let mut row = like_row("aa".repeat(32).as_str());
        row.source = "bluesky".into();
        row.subject_uri = Some("at://did:plc:xyz/app.bsky.feed.post/3kpost".into());
        app.notifications.items = vec![row.clone()];
        let painted = elements(&app);
        let item_el = painted
            .iter()
            .find(|e| e.id == ids::NOTIFICATION_ITEM)
            .expect("the row paints");
        match &item_el.role {
            Role::Button(Gesture::OpenNotification(NotificationDestination::External { url })) => {
                assert_eq!(url, "https://bsky.app/profile/did:plc:xyz/post/3kpost");
            }
            other => panic!(
                "a bridged like with a post subject must offer the off-app open, got {other:?}"
            ),
        }

        row.subject_uri = None;
        app.notifications.items = vec![row];
        let painted = elements(&app);
        let item_el = painted
            .iter()
            .find(|e| e.id == ids::NOTIFICATION_ITEM)
            .expect("the row paints");
        assert!(
            matches!(item_el.role, Role::Label),
            "a bridged row's content_id is a dedup token — without a subject it must not offer a tap"
        );
    }

    /// The ward-side doorbell goes where its three guardian-side siblings go
    /// (user-ruled 2026-09-25) — and, like them, only while the Family tab is
    /// gated in for this session.
    #[test]
    fn the_ward_side_feed_source_approval_opens_the_family_page_when_gated_in() {
        use crate::element::Role;
        let mut app = app_ready_to_navigate();
        let mut row = item(
            "family.feed_source_approved",
            "your guardian approved the source — try again",
            false,
        );
        row.content_id = Some("3432".into());
        app.notifications.items = vec![row];

        app.has_family = false;
        let painted = elements(&app);
        let el = painted
            .iter()
            .find(|e| e.id == ids::NOTIFICATION_ITEM)
            .expect("the row paints");
        assert!(matches!(el.role, Role::Label), "no family-tab, no tap");

        app.has_family = true;
        let painted = elements(&app);
        let el = painted
            .iter()
            .find(|e| e.id == ids::NOTIFICATION_ITEM)
            .expect("the row paints");
        assert!(
            matches!(
                el.role,
                Role::Button(Gesture::OpenNotification(NotificationDestination::Family))
            ),
            "a ward's approval doorbell opens the Family page"
        );
    }

    /// The dispatcher-level twin: `Gesture::OpenNotification` must reach
    /// `open_notification` through the one gesture door (`app::gesture_work`),
    /// the same door the row's click paints through — a unit test calling
    /// `open_notification` directly cannot catch a missing match arm there.
    #[test]
    fn the_gesture_reaches_the_liked_post_through_the_gesture_door() {
        let mut app = app_ready_to_navigate();
        let work = crate::app::gesture_work(
            &mut app,
            Gesture::OpenNotification(NotificationDestination::Post {
                post_id: "p7".into(),
            }),
        );
        assert!(!matches!(work, crate::app::GestureWork::None));
        assert_eq!(app.page, Page::Feed);
        assert_eq!(app.feed.mode, crate::feed::Mode::PostDetail("p7".into()));
    }

    /// A knock lands on the Contacts page's **People** segment — the sticky
    /// segment may be the Address Book, where `knock-request-item` does not
    /// render at all.
    #[test]
    fn a_knock_opens_the_people_segment_of_contacts() {
        let mut app = app_ready_to_navigate();
        app.contacts.segment = crate::address_book::Segment::AddressBook;
        crate::app::gesture_work(
            &mut app,
            Gesture::OpenNotification(NotificationDestination::Knock {
                sender_id: "cc".repeat(32),
            }),
        );
        assert_eq!(app.page, Page::Contacts);
        assert_eq!(app.contacts.segment, crate::address_book::Segment::People);
    }

    /// A family doorbell opens the Family page — but only while the tab is
    /// actually there: a doorbell outliving its family link (a graduated
    /// ward's) must not offer a control that navigates to a page with no
    /// sidebar row.
    #[test]
    fn a_family_doorbell_paints_only_while_the_family_tab_is_gated_in() {
        use crate::element::Role;
        let mut app = app_ready_to_navigate();
        app.notifications.items = vec![item(
            "family.contact_request",
            "your ward asked to add a contact",
            false,
        )];

        app.has_family = false;
        let inert = elements(&app);
        let row = inert
            .iter()
            .find(|e| e.id == ids::NOTIFICATION_ITEM)
            .expect("the row paints");
        assert!(
            matches!(row.role, Role::Label),
            "no family-tab, no tap — the destination page has no sidebar row"
        );

        app.has_family = true;
        let live = elements(&app);
        let row = live
            .iter()
            .find(|e| e.id == ids::NOTIFICATION_ITEM)
            .expect("the row paints");
        assert!(
            matches!(row.role, Role::Button(_)),
            "a guardian's doorbell opens the Family page"
        );

        crate::app::gesture_work(
            &mut app,
            Gesture::OpenNotification(NotificationDestination::Family),
        );
        assert_eq!(app.page, Page::Family);
    }
}
