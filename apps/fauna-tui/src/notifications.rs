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
    notification_text,
    notifications::{NotifItem, NotifType},
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

/// The page's own gesture (`behavior/notifications.md` § User actions). Row
/// activation is NOT here — it navigates to another page, so it rides
/// [`crate::element::Gesture::OpenNotification`] and
/// [`open_notification`], the `OpenSearchResult` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MarkAllRead,
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`).
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // `notifications_mark_read(None)` — the whole-list form.
            Action::MarkAllRead => Some("fauna.notifications.mark_read"),
        }
    }
}

/// Local half of a gesture → its network half (the feed/conversations split:
/// the agent awaits the op, the keyboard spawns it).
pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    let st = &app.notifications;
    match action {
        Action::MarkAllRead => Some(Op::MarkAllRead {
            nest: st.nest.clone()?,
        }),
    }
}

/// The network half. Every variant refetches after its mutation — the reply is
/// a bare count echo, so the fresh list + count ARE the observable effect
/// (the contacts refetch-after-mutate convention).
pub enum Op {
    Refresh { nest: Arc<NestClient> },
    MarkAllRead { nest: Arc<NestClient> },
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
        _ => "•",
    }
}

/// A row's relative timestamp — the tui-wide formatter (`crate::format`);
/// `created_at` is epoch-**micro**seconds on the wire.
fn format_epoch_us(us: i64) -> String {
    crate::format::format_epoch_us(us)
}

/// The ordered ui.yaml element list (page `notifications` + the
/// `notification-row` component: `notification-item` + `notification-type-icon`,
/// both FLAT indexed ids — one per row in registration order, like the
/// conversations bubble children).
pub fn elements(app: &App) -> Vec<Element> {
    let st = &app.notifications;
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
        let text = if when.is_empty() {
            summary
        } else {
            format!("{summary}  ·  {when}")
        };
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

    /// ui.yaml `notifications`: heading, badge, mark-read; then one
    /// (type-icon, item) pair per row in registration order — flat indexed ids.
    #[test]
    fn the_page_paints_exactly_the_ui_yaml_elements() {
        let mut app = crate::app::tests::test_app();
        let ids: Vec<String> = elements(&app).iter().map(|e| e.id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                "page-heading",
                "notification-count-badge",
                "notification-mark-read"
            ],
            "the empty page paints the three chrome elements and no rows"
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
                "notification-type-icon",
                "notification-item",
            ],
        );
        assert!(els[1].text.contains('1'), "badge shows the nest count");
        assert_eq!(els[3].text, "♥");
        assert!(els[4].text.starts_with("A liked your post"));
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
            els[4]
                .text
                .starts_with(&fauna_i18n::strings::notifications::row_like("alice")),
            "painted {:?}",
            els[4].text
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
        assert!(elements(&app)[4].text.starts_with("ENGLISH FALLBACK"));
    }

    /// An empty server summary falls back to the shared default body — the
    /// linux row's exact fallback (`views/notifications.rs`).
    #[test]
    fn empty_summary_falls_back_to_the_shared_default() {
        let mut app = crate::app::tests::test_app();
        app.notifications.items = vec![item("reply", "", true)];
        let els = elements(&app);
        assert!(
            els[4]
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
