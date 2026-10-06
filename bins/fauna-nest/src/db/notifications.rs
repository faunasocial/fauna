//! Unified notification storage: insert, list, mark-read, count.

use super::CacheDb;
use anyhow::{Context, Result};
use fauna_protocol::LocalizedText;
use fauna_protocol::notifications::{NotifType, security_notice_window_start};

/// What a notification says, in the two forms the nest stores it
/// (`behavior/notifications.md` § Localized body): the catalog key + data args
/// the app localizes, and the same sentence in English as the compat fallback
/// an older app — or an app that does not have the key — paints.
///
/// [`CacheDb::insert_notification`] takes this rather than a bare `&str` so a
/// new producer cannot compose an English sentence and forget the key: the
/// only keyless constructors are the test hook's and `#[cfg(test)]`'s.
#[derive(Debug, Clone)]
pub struct NotificationText {
    body: Option<LocalizedText>,
    summary: String,
}

impl NotificationText {
    /// A producer's constructor. `body.key` names an `en.yaml`
    /// `notifications.row_*` entry; `summary` is that entry's English
    /// rendering with the same args.
    pub fn new(body: LocalizedText, summary: impl Into<String>) -> Self {
        Self {
            body: Some(body),
            summary: summary.into(),
        }
    }

    /// The ordinary producer's constructor: `summary` is the catalog's own
    /// English rendering of `body`, so the nest carries no copy of the sentence
    /// to drift from `en.yaml`. (`fauna_i18n` is the English catalog; the nest
    /// never localizes — it has no reader locale to localize to.)
    pub fn localized(body: LocalizedText) -> Self {
        let mut summary = fauna_i18n::strings::lookup(&body.key)
            .unwrap_or_default()
            .to_string();
        debug_assert!(!summary.is_empty(), "no catalog entry for {}", body.key);
        for (name, value) in &body.args {
            summary = summary.replace(&format!("{{{name}}}"), value);
        }
        Self {
            body: Some(body),
            summary,
        }
    }

    /// `push_test_hooks` only: an e2e test names the summary, and the body only
    /// when it is the body under test. Compiled out with the hook itself.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn from_test_hook(body: Option<LocalizedText>, summary: String) -> Self {
        Self { body, summary }
    }

    /// A fixture row whose text is not under test.
    #[cfg(test)]
    pub(crate) fn untranslated(summary: &str) -> Self {
        Self {
            body: None,
            summary: summary.to_string(),
        }
    }

    pub fn body(&self) -> Option<&LocalizedText> {
        self.body.as_ref()
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// A producer's body must name a catalog entry and fill every placeholder
/// in it: a missing key renders as the English summary on every app for ever,
/// and an unfilled `{name}` renders as a hole. Each producer's tests run its
/// bodies through this.
#[cfg(test)]
pub(crate) fn assert_body_is_catalog_complete(body: &LocalizedText) {
    let template = fauna_i18n::strings::lookup(&body.key)
        .unwrap_or_else(|| panic!("{} is not in i18n/strings/en.yaml", body.key));
    let rendered = NotificationText::localized(body.clone());
    assert!(
        !rendered.summary().contains('{'),
        "{} leaves a placeholder unfilled: {template:?} rendered {:?}",
        body.key,
        rendered.summary()
    );
}

/// A single notification row.
#[derive(Debug)]
pub struct NotificationRow {
    pub id: i64,
    pub actor_id: Vec<u8>,
    /// Read back through [`NotifType::from`]: a type this build does not
    /// name is carried, never refused, so the list never fails on one row.
    pub notif_type: NotifType,
    pub source: String,
    pub sender_id: Option<Vec<u8>>,
    pub content_id: Option<Vec<u8>>,
    pub subject_uri: Option<String>,
    pub summary: String,
    /// `None` on a row written with no body (NULL `body_key`).
    pub body: Option<LocalizedText>,
    pub is_read: bool,
    pub created_at: i64,
}

/// What [`CacheDb::dismiss_notification`] did with the named row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DismissOutcome {
    /// The row was the caller's and went.
    Dismissed,
    /// No row of that id is the caller's (never was, or already gone).
    NotFound,
    /// The row is the caller's `security.notice`, still inside its window.
    Retained,
}

/// Rebuild the stored body. A `body_args` that does not parse as a JSON object
/// of strings drops the whole body — the row then renders its `summary`, which
/// is always whole, rather than a sentence with holes in it.
fn body_from_columns(key: Option<String>, args: Option<String>) -> Option<LocalizedText> {
    let mut body = LocalizedText::new(key?);
    if let Some(args) = args {
        body.args = serde_json::from_str(&args).ok()?;
    }
    Some(body)
}

impl CacheDb {
    /// Insert a notification. Returns the row ID.
    /// Deduplicates on (actor_id, notif_type, sender_id, content_id) -- if an
    /// identical notification already exists, this is a no-op and returns None.
    pub async fn insert_notification(
        &self,
        actor_id: &[u8],
        notif_type: &NotifType,
        source: &str,
        sender_id: Option<&[u8]>,
        content_id: Option<&[u8]>,
        subject_uri: Option<&str>,
        text: &NotificationText,
        created_at: i64,
    ) -> Result<Option<i64>> {
        let actor_id = actor_id.to_vec();
        let notif_type = notif_type.as_wire().to_string();
        let source = source.to_string();
        let sender_id = sender_id.map(|s| s.to_vec());
        let content_id = content_id.map(|c| c.to_vec());
        let subject_uri = subject_uri.map(|s| s.to_string());
        let summary = text.summary.clone();
        let body_key = text.body.as_ref().map(|b| b.key.clone());
        let body_args = match &text.body {
            Some(b) => Some(serde_json::to_string(&b.args).context("encode body args")?),
            None => None,
        };
        let conn = self.conn.lock().await;

        // Check for duplicate
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM notifications
                 WHERE actor_id = ?1 AND notif_type = ?2
                   AND sender_id IS ?3 AND content_id IS ?4",
                rusqlite::params![actor_id, notif_type, sender_id, content_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);

        if exists {
            return Ok(None);
        }

        conn.execute(
            "INSERT INTO notifications
                (actor_id, notif_type, source, sender_id, content_id,
                 subject_uri, summary, is_read, created_at, body_key, body_args)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10)",
            rusqlite::params![
                actor_id,
                notif_type,
                source,
                sender_id,
                content_id,
                subject_uri,
                summary,
                created_at,
                body_key,
                body_args,
            ],
        )
        .context("insert notification")?;

        let row_id = conn.last_insert_rowid();
        Ok(Some(row_id))
    }

    /// List notifications for an actor, ordered by created_at DESC.
    /// Supports cursor-based pagination (cursor = last seen `id`).
    pub async fn list_notifications(
        &self,
        actor_id: &[u8],
        cursor: Option<i64>,
        limit: i64,
    ) -> Result<Vec<NotificationRow>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;

        let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match cursor {
            Some(c) => (
                "SELECT id, actor_id, notif_type, source, sender_id, content_id,
                        subject_uri, summary, is_read, created_at, body_key, body_args
                 FROM notifications
                 WHERE actor_id = ?1 AND id < ?2
                 ORDER BY created_at DESC
                 LIMIT ?3",
                vec![
                    Box::new(actor_id) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(c),
                    Box::new(limit),
                ],
            ),
            None => (
                "SELECT id, actor_id, notif_type, source, sender_id, content_id,
                        subject_uri, summary, is_read, created_at, body_key, body_args
                 FROM notifications
                 WHERE actor_id = ?1
                 ORDER BY created_at DESC
                 LIMIT ?2",
                vec![
                    Box::new(actor_id) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(limit),
                ],
            ),
        };

        let mut stmt = conn.prepare(sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(NotificationRow {
                    id: row.get(0)?,
                    actor_id: row.get(1)?,
                    notif_type: NotifType::from(row.get::<_, String>(2)?),
                    source: row.get(3)?,
                    sender_id: row.get(4)?,
                    content_id: row.get(5)?,
                    subject_uri: row.get(6)?,
                    summary: row.get(7)?,
                    body: body_from_columns(row.get(10)?, row.get(11)?),
                    is_read: row.get::<_, i64>(8)? != 0,
                    created_at: row.get(9)?,
                })
            })
            .context("list notifications")?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Mark all notifications as read for an actor up to a given timestamp.
    pub async fn mark_notifications_read(&self, actor_id: &[u8], up_to: i64) -> Result<u64> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let count = conn
            .execute(
                "UPDATE notifications SET is_read = 1
                 WHERE actor_id = ?1 AND created_at <= ?2 AND is_read = 0",
                rusqlite::params![actor_id, up_to],
            )
            .context("mark notifications read")?;
        Ok(count as u64)
    }

    /// Count unread notifications for an actor.
    pub async fn count_unread_notifications(&self, actor_id: &[u8]) -> Result<i64> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let count = conn
            .query_row(
                "SELECT COUNT(*) FROM notifications
                 WHERE actor_id = ?1 AND is_read = 0",
                rusqlite::params![actor_id],
                |row| row.get(0),
            )
            .context("count unread notifications")?;
        Ok(count)
    }

    // ── Deletes (`behavior/notifications.md` § Retention) ──────────────
    //
    // A notification row is the user's own record of what others did toward
    // their account; nothing sweeps it by age or count. It leaves the table
    // with the account (`actor_tables.rs`, `Policy::Purge`), by the user's
    // hand (the two methods below, behind `fauna.notifications.{dismiss,
    // clear}`), or — the `knock` row only — with the knock it announces
    // (`delete_knock_notification`, plus the join inside `expire_old_knocks`).

    /// Delete one row by id, scoped to `actor_id`: an id that is another
    /// actor's row is not found, never someone else's delete. Idempotent — a
    /// replay or a double-tap deletes nothing new and is not an error. A
    /// `security.notice` row inside its window at `now` (micros) is
    /// [`DismissOutcome::Retained`], not deleted (§ Retention, the
    /// security-notice window).
    pub async fn dismiss_notification(
        &self,
        actor_id: &[u8],
        id: i64,
        now: i64,
    ) -> Result<DismissOutcome> {
        let actor_id = actor_id.to_vec();
        let window_start = security_notice_window_start(now);
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM notifications WHERE actor_id = ?1 AND id = ?2
                   AND NOT (notif_type = ?3 AND created_at > ?4)",
                rusqlite::params![
                    actor_id,
                    id,
                    NotifType::SecurityNotice.as_wire(),
                    window_start
                ],
            )
            .context("dismiss notification")?;
        if deleted > 0 {
            return Ok(DismissOutcome::Dismissed);
        }
        // Nothing went: tell "yours but retained" from "not yours / gone".
        // Same lock, so no delete can land between the two statements.
        let retained: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM notifications WHERE actor_id = ?1 AND id = ?2)",
                rusqlite::params![actor_id, id],
                |row| row.get(0),
            )
            .context("dismiss notification: probe retained")?;
        Ok(if retained {
            DismissOutcome::Retained
        } else {
            DismissOutcome::NotFound
        })
    }

    /// Delete every row of `actor_id` created at or before `up_to` (micros),
    /// skipping any `security.notice` row still inside its window at `now`.
    /// Returns the number deleted. The bound is what lets a page that listed
    /// and then cleared leave a row that arrived after it looked.
    pub async fn clear_notifications(&self, actor_id: &[u8], up_to: i64, now: i64) -> Result<u64> {
        let actor_id = actor_id.to_vec();
        let window_start = security_notice_window_start(now);
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM notifications WHERE actor_id = ?1 AND created_at <= ?2
                   AND NOT (notif_type = ?3 AND created_at > ?4)",
                rusqlite::params![
                    actor_id,
                    up_to,
                    NotifType::SecurityNotice.as_wire(),
                    window_start
                ],
            )
            .context("clear notifications")?;
        Ok(deleted as u64)
    }

    /// Delete the `knock` doorbell(s) `sender_id` rang at `actor_id` — the
    /// row `store_knock` writes beside a knock. Called where the knock itself
    /// goes by the user's hand (`fauna.knocks.{dismiss,block}`) and before a
    /// re-knock writes its fresh doorbell; NOT on accept, where the doorbell
    /// becomes the record of an accepted request. Recreatable in the one
    /// sense that matters: the knock it announced is gone or about to be
    /// re-rung, and the knock row is what the product already expires.
    pub async fn delete_knock_notification(
        &self,
        actor_id: &[u8; 32],
        sender_id: &[u8; 32],
    ) -> Result<u64> {
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM notifications
                 WHERE actor_id = ?1 AND notif_type = 'knock' AND sender_id = ?2",
                rusqlite::params![actor_id.as_slice(), sender_id.as_slice()],
            )
            .context("delete knock notification")?;
        Ok(deleted as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> CacheDb {
        CacheDb::open_in_memory().unwrap()
    }

    #[tokio::test]
    async fn insert_and_list_notifications() {
        let db = test_db().await;
        let actor = [1u8; 32];
        let sender = [2u8; 32];
        let content = [3u8; 32];

        // Insert a like notification
        let id = db
            .insert_notification(
                &actor,
                &fauna_protocol::notifications::NotifType::Like,
                "fauna",
                Some(&sender),
                Some(&content),
                None,
                &NotificationText::untranslated("alice liked your post"),
                1000,
            )
            .await
            .unwrap();
        assert!(id.is_some());

        // List should return it
        let notifs = db.list_notifications(&actor, None, 10).await.unwrap();
        assert_eq!(notifs.len(), 1);
        assert_eq!(notifs[0].notif_type, NotifType::Like);
        assert_eq!(notifs[0].summary, "alice liked your post");
        assert!(!notifs[0].is_read);
    }

    /// `behavior/notifications.md` § Localized body → *At rest*: the key and
    /// its args survive the two columns, beside the English fallback.
    #[tokio::test]
    async fn the_localized_body_round_trips_beside_its_summary() {
        let db = test_db().await;
        let actor = [1u8; 32];
        let body = LocalizedText::new("notifications.row_like").with_arg("sender", "alice");
        db.insert_notification(
            &actor,
            &fauna_protocol::notifications::NotifType::Like,
            "bluesky",
            None,
            Some(b"at://x"),
            None,
            &NotificationText::new(body.clone(), "alice liked your post"),
            1000,
        )
        .await
        .unwrap();

        let rows = db.list_notifications(&actor, None, 10).await.unwrap();
        assert_eq!(rows[0].body.as_ref(), Some(&body));
        assert_eq!(rows[0].summary, "alice liked your post");
    }

    /// A row written with no body (a `NotificationText` that carries none —
    /// the test hook's and the fixtures' shape) has NULLs in both columns: no
    /// body, and its `summary` is what renders.
    #[tokio::test]
    async fn a_row_with_null_body_columns_reads_back_without_a_body() {
        let db = test_db().await;
        let actor = [1u8; 32];
        db.insert_notification(
            &actor,
            &fauna_protocol::notifications::NotifType::Like,
            "fauna",
            None,
            None,
            None,
            &NotificationText::untranslated("alice liked your post"),
            1000,
        )
        .await
        .unwrap();

        let rows = db.list_notifications(&actor, None, 10).await.unwrap();
        assert!(rows[0].body.is_none());
        assert_eq!(rows[0].summary, "alice liked your post");
    }

    /// Args that are not a JSON object of strings drop the whole body rather
    /// than yield a sentence with holes — the summary is always whole.
    #[test]
    fn unparseable_args_drop_the_body_not_just_the_args() {
        assert!(
            body_from_columns(
                Some("notifications.row_like".into()),
                Some("not json".into())
            )
            .is_none()
        );
        assert!(body_from_columns(None, Some("{}".into())).is_none());
        let keyed = body_from_columns(Some("notifications.row_like".into()), None).unwrap();
        assert!(keyed.args.is_empty());
    }

    #[tokio::test]
    async fn dedup_prevents_duplicate_notification() {
        let db = test_db().await;
        let actor = [1u8; 32];
        let sender = [2u8; 32];
        let content = [3u8; 32];

        let id1 = db
            .insert_notification(
                &actor,
                &fauna_protocol::notifications::NotifType::Like,
                "fauna",
                Some(&sender),
                Some(&content),
                None,
                &NotificationText::untranslated("alice liked your post"),
                1000,
            )
            .await
            .unwrap();
        assert!(id1.is_some());

        // Same notification again should be deduplicated
        let id2 = db
            .insert_notification(
                &actor,
                &fauna_protocol::notifications::NotifType::Like,
                "fauna",
                Some(&sender),
                Some(&content),
                None,
                &NotificationText::untranslated("alice liked your post"),
                2000,
            )
            .await
            .unwrap();
        assert!(id2.is_none());

        let notifs = db.list_notifications(&actor, None, 10).await.unwrap();
        assert_eq!(notifs.len(), 1);
    }

    #[tokio::test]
    async fn mark_read_and_count() {
        let db = test_db().await;
        let actor = [1u8; 32];

        // Insert two notifications
        db.insert_notification(
            &actor,
            &fauna_protocol::notifications::NotifType::Like,
            "fauna",
            Some(&[2u8; 32]),
            Some(&[3u8; 32]),
            None,
            &NotificationText::untranslated("notif 1"),
            1000,
        )
        .await
        .unwrap();

        db.insert_notification(
            &actor,
            &fauna_protocol::notifications::NotifType::Reply,
            "fauna",
            Some(&[4u8; 32]),
            Some(&[5u8; 32]),
            None,
            &NotificationText::untranslated("notif 2"),
            2000,
        )
        .await
        .unwrap();

        // Both should be unread
        let count = db.count_unread_notifications(&actor).await.unwrap();
        assert_eq!(count, 2);

        // Mark read up to timestamp 1500 (only first one)
        let marked = db.mark_notifications_read(&actor, 1500).await.unwrap();
        assert_eq!(marked, 1);

        let count = db.count_unread_notifications(&actor).await.unwrap();
        assert_eq!(count, 1);

        // Mark all read
        let marked = db.mark_notifications_read(&actor, 3000).await.unwrap();
        assert_eq!(marked, 1);

        let count = db.count_unread_notifications(&actor).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn cursor_pagination() {
        let db = test_db().await;
        let actor = [1u8; 32];

        // Insert 5 notifications
        for i in 0..5 {
            db.insert_notification(
                &actor,
                &fauna_protocol::notifications::NotifType::Like,
                "fauna",
                Some(&[(i + 10) as u8; 32]),
                Some(&[(i + 20) as u8; 32]),
                None,
                &NotificationText::untranslated(&format!("notif {i}")),
                (i as i64 + 1) * 1000,
            )
            .await
            .unwrap();
        }

        // First page: 3 items
        let page1 = db.list_notifications(&actor, None, 3).await.unwrap();
        assert_eq!(page1.len(), 3);

        // Second page: use last ID as cursor
        let cursor = page1.last().unwrap().id;
        let page2 = db
            .list_notifications(&actor, Some(cursor), 3)
            .await
            .unwrap();
        assert_eq!(page2.len(), 2);
    }
}
