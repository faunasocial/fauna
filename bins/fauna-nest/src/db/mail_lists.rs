//! Mailing-list storage (`mail_lists` / `mail_list_members`) +
//! the per-deployment one-click-unsubscribe secret
//! (`mail_list_unsubscribe_secrets`).
//!
//! Backs `docs/goal/behavior/mail-mass-mailing.md`. The table rows + the
//! sixth `account_aliases.kind = 'list'` alias are defined in
//! `migrations::MIGRATIONS_MAIL_LISTS`; the unsubscribe secret in
//! `migrations::MIGRATIONS_MAIL_LIST_UNSUBSCRIBE_SECRET` (auto-seeded on first
//! DB open).
//!
//! The unsubscribe secret is **nest-held plaintext**, the same class +
//! lifecycle as the SRS secret ([`super::mail_srs`]): the goal doc § Token
//! format puts it "in nest state ... server-managed", and the MTA never needs
//! it (the unsubscribe handlers resolve a token by the cached
//! `mail_list_members.one_click_unsubscribe_token` index, never by re-deriving
//! it MTA-side). The nest constructs a `fauna_mail::lists::
//! UnsubscribeTokenGenerator` from [`CacheDb::get_active_list_unsubscribe_secret`]
//! to derive a member's token at subscribe/send time.
//!
//! **Timestamps are epoch-millis** — the convention of the whole mail
//! subsystem (`mail_domains`, `account_aliases`), *not* the "Unix-seconds" the
//! `MIGRATIONS_MAIL_LISTS` comment mistakenly cited. A list's `created_at` and
//! a member's `subscribed_at` / `unsubscribed_at` all share that basis, and the
//! `kind = 'list'` `account_aliases` row this layer inserts uses the same
//! `now_epoch_millis()` as every other alias row, so the alias and its list row
//! never disagree on units.
//!
//! Items #3 + #10a: the secret accessor and
//! the list/member CRUD (the `fauna.bridges.*_account_list` family, the member
//! sub-surface, and the by-token one-click unsubscribe). The list-mode
//! submission discriminator, the four-scope rate accounting, `send_list_message`,
//! and secret rotation land with their consuming items (#6–#11).

use super::{CacheDb, blob_col_to_array, blob_to_array, now_epoch_millis};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use uuid::Uuid;

/// One `mail_lists` row joined with its `account_aliases` row for the
/// address (`local_domain` + `pattern`). The wire `MailListRow` is built from
/// this in the handler.
#[derive(Debug, Clone, PartialEq)]
pub struct MailListRecord {
    pub list_id: [u8; 16],
    pub alias_id: [u8; 16],
    /// 32-byte owning actor id.
    pub owner_actor_id: Vec<u8>,
    /// From the joined `account_aliases` row (the list's posting address).
    pub local_domain: String,
    pub pattern: String,
    pub friendly_name: Option<String>,
    pub description: Option<String>,
    pub list_help_url: Option<String>,
    pub list_archive_url: Option<String>,
    pub recipients_per_send: Option<i64>,
    pub created_at: i64,
    pub last_send_at: Option<i64>,
    pub member_count: i64,
    pub sends_today: i64,
    pub recipients_today: i64,
}

/// One `mail_list_members` row, owner-facing (the cached
/// `one_click_unsubscribe_token` is deliberately omitted — it is an
/// implementation detail of the unsubscribe handlers, never surfaced to the
/// list owner's UI).
#[derive(Debug, Clone, PartialEq)]
pub struct MailListMemberRecord {
    pub member_id: [u8; 16],
    pub recipient_address: String,
    pub subscribed_at: i64,
    /// `None` = subscribed; `Some(ts)` = unsubscribed at that epoch-millis.
    pub unsubscribed_at: Option<i64>,
}

/// Why a `create_list` write failed. The handler maps `Conflict` →
/// `fauna.bridges.conflicts_with_existing_alias` (the same wire code the
/// alias CRUD uses for a `(local_domain, pattern, kind)` collision) and
/// `Other` → `fauna.protocol.internal`.
#[derive(Debug)]
pub enum ListWriteError {
    /// The list's posting address collides with an existing list on
    /// `(local_domain, pattern, kind='list')`, or with the exact alias or
    /// forwarder that already holds the key.
    Conflict,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for ListWriteError {
    fn from(e: anyhow::Error) -> Self {
        ListWriteError::Other(e)
    }
}

/// Outcome of subscribing one address (idempotent on a duplicate). The
/// member_id is always returned so the handler can echo it regardless of
/// whether the row was freshly inserted.
#[derive(Debug, Clone, PartialEq)]
pub enum AddMemberOutcome {
    /// A new subscription row was inserted.
    Added([u8; 16]),
    /// `(list_id, recipient_address)` already existed — left untouched
    /// (subscription is sticky; re-subscribing an unsubscribed member is the
    /// explicit `resubscribe_list_member` path, never an implicit `add`).
    AlreadyExists([u8; 16]),
}

impl AddMemberOutcome {
    pub fn member_id(&self) -> [u8; 16] {
        match self {
            AddMemberOutcome::Added(id) | AddMemberOutcome::AlreadyExists(id) => *id,
        }
    }
}

/// Effective per-list-send caps the quota check enforces (§ Per-list rate
/// accounting). Resolved by the handler from the list's `recipients_per_send`
/// override + the admin-tunable [`fauna_protocol::bridge_routing::MassMailingPolicy`]
/// ceilings, so the DB layer takes concrete numbers and never re-reads policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListSendCaps {
    /// Max recipients in this one send (the per-list override already bounded
    /// by the admin ceiling). Over-cap is a hard reject (`552 5.3.4`).
    pub per_send: i64,
    /// Per-account-per-day recipient ceiling across all the owner's lists.
    /// Over-cap is a tempfail (`452 4.7.0`).
    pub per_account_per_day: i64,
    /// Deployment-wide per-day recipient ceiling (the abuse safety valve).
    /// Over-cap is a tempfail (`452`).
    pub per_deployment_per_day: i64,
}

/// Outcome of reserving quota for one list send (`try_consume_list_quota`). The
/// three rejections map to the goal doc's SMTP codes — `552` hard for per-send,
/// `452` tempfail for the two daily ceilings — surfaced by the
/// `send_list_message` RPC as structured errors. On any reject **no counter is
/// touched** (the whole send is refused atomically).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListQuotaOutcome {
    /// All caps pass; the per-list meters + the account/deployment day counters
    /// were incremented by `recipient_count`. `account_remaining` is the
    /// owner's per-day headroom left after this send (the RPC's
    /// `estimated_quota_remaining`).
    Allowed {
        account_remaining: i64,
        deployment_remaining: i64,
    },
    /// `recipient_count` exceeds the per-send cap (`552 5.3.4`, no auto-chunk).
    PerSendExceeded { cap: i64 },
    /// The per-account-per-day ceiling would be exceeded (`452 4.7.0`).
    PerAccountDayExceeded { cap: i64, remaining: i64 },
    /// The deployment-wide per-day ceiling would be exceeded (`452`).
    PerDeploymentDayExceeded { cap: i64, remaining: i64 },
}

/// One subscribed member's address + its cached one-click token — the per-send
/// fan-out input (`send_list_message`). Unlike [`MailListMemberRecord`] this
/// carries the token (each recipient's `List-Unsubscribe` header needs it), so
/// it is an internal send-path shape, never returned to the owner's UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListSendMember {
    pub recipient_address: String,
    /// The cached `one_click_unsubscribe_token`; empty only on the unreachable
    /// path where a member row carries no token (the member INSERT and every
    /// rotation write one; the skip is a fail-closed guard).
    pub one_click_unsubscribe_token: String,
}

/// One `mail_list_sends` history row (`list_list_send_history`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailListSendRecord {
    pub send_id: [u8; 16],
    pub sent_at: i64,
    pub recipient_count: i64,
    pub delivered_count: i64,
    pub unsubscribed_during_send: i64,
}

/// Outcome of the by-token one-click unsubscribe (HTTPS / mailto handlers).
#[derive(Debug, Clone, PartialEq)]
pub enum UnsubscribeOutcome {
    /// No member row carries this token (404 on the HTTPS endpoint).
    NotFound,
    /// The member was subscribed; now unsubscribed (200).
    Unsubscribed,
    /// The member was already unsubscribed; idempotent no-op (200
    /// "Already unsubscribed.").
    AlreadyUnsubscribed,
}

/// Columns of a `mail_lists` row joined with `account_aliases` for the address,
/// in the order [`row_to_list_record`] reads them.
const LIST_SELECT_COLS: &str = "ml.list_id, ml.alias_id, ml.owner_actor_id, \
     a.local_domain, a.pattern, ml.list_friendly_name, ml.description, \
     ml.list_help_url, ml.list_archive_url, ml.recipients_per_send, \
     ml.created_at, ml.last_send_at, ml.member_count, ml.sends_today, \
     ml.recipients_today";

fn row_to_list_record(row: &rusqlite::Row) -> rusqlite::Result<MailListRecord> {
    Ok(MailListRecord {
        list_id: blob_col_to_array(row.get(0)?, 0, "list_id")?,
        alias_id: blob_col_to_array(row.get(1)?, 1, "alias_id")?,
        owner_actor_id: row.get(2)?,
        local_domain: row.get(3)?,
        pattern: row.get(4)?,
        friendly_name: row.get(5)?,
        description: row.get(6)?,
        list_help_url: row.get(7)?,
        list_archive_url: row.get(8)?,
        recipients_per_send: row.get(9)?,
        created_at: row.get(10)?,
        last_send_at: row.get(11)?,
        member_count: row.get(12)?,
        sends_today: row.get(13)?,
        recipients_today: row.get(14)?,
    })
}

impl CacheDb {
    /// The active (newest) one-click-unsubscribe secret — the 32-byte HMAC key
    /// behind every member's `one_click_unsubscribe_token`. Auto-seeded on
    /// first DB open, so `None` only if that seed somehow didn't run (callers
    /// treat that as "mass-mailing unavailable" and skip token derivation).
    /// Rotation (item #11) inserts a fresh row + re-tokenizes every member, so
    /// the newest row is always the one in-flight tokens were minted under.
    pub async fn get_active_list_unsubscribe_secret(&self) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let secret: Option<Vec<u8>> = conn
            .query_row(
                "SELECT secret FROM mail_list_unsubscribe_secrets ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        Ok(secret)
    }

    // ── list CRUD (`fauna.bridges.{list,create,update,delete}_account_list`) ──

    /// Create one list owned by `owner`: insert a `kind='list'` `account_aliases`
    /// row for the posting address **and** the `mail_lists` row referencing it,
    /// atomically (§ The list as an alias row). `local_domain` + `pattern` are
    /// lower-cased (the resolver compares `LOWER(...)`); a
    /// `UNIQUE (local_domain, pattern, kind)` collision — or an exact alias or
    /// forwarder already on the address — surfaces as
    /// [`ListWriteError::Conflict`]. Returns `(list_id, alias_id)`.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_list(
        &self,
        owner: &[u8; 32],
        local_domain: &str,
        pattern: &str,
        friendly_name: Option<&str>,
        description: Option<&str>,
        list_help_url: Option<&str>,
        list_archive_url: Option<&str>,
        recipients_per_send: Option<i64>,
    ) -> std::result::Result<([u8; 16], [u8; 16]), ListWriteError> {
        let local_domain = local_domain.to_ascii_lowercase();
        let pattern = pattern.to_ascii_lowercase();
        let owner = owner.to_vec();
        let alias_id = *Uuid::new_v4().as_bytes();
        let list_id = *Uuid::new_v4().as_bytes();
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction()
            .context("begin create_list transaction")?;
        // One holder per exact-tier key: an exact alias or forwarder already on
        // the address is a `kind` the UNIQUE index cannot see.
        if super::mail_aliases::exact_key_held_by_other_kind(
            &tx,
            &local_domain,
            &pattern,
            super::mail_aliases::ALIAS_KIND_LIST,
        )? {
            return Err(ListWriteError::Conflict);
        }
        // The `kind='list'` alias row — the posting address. Same column set +
        // millis `created_at` as every other alias (`create_account_alias`), so
        // list rows are uniform with the table; the controls are inert for a
        // list (label empty, never rate-limited inbound — lists are outbound).
        tx.execute(
            "INSERT INTO account_aliases
                (alias_id, actor_id, local_domain, kind, pattern, label,
                 disabled, spam_threshold_override, rate_limit_per_hour,
                 rate_limit_per_day, hit_count, created_at)
             VALUES (?1, ?2, ?3, 'list', ?4, '', 0, NULL, NULL, NULL, 0, ?5)",
            rusqlite::params![&alias_id[..], &owner[..], &local_domain, &pattern, now],
        )
        .map_err(map_list_write_err)?;
        tx.execute(
            "INSERT INTO mail_lists
                (list_id, alias_id, owner_actor_id, list_friendly_name,
                 description, list_help_url, list_archive_url,
                 recipients_per_send, created_at, last_send_at, member_count,
                 sends_today, recipients_today)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, 0, 0, 0)",
            rusqlite::params![
                &list_id[..],
                &alias_id[..],
                &owner[..],
                friendly_name,
                description,
                list_help_url,
                list_archive_url,
                recipients_per_send,
                now,
            ],
        )
        .map_err(map_list_write_err)?;
        tx.commit().context("commit create_list")?;
        Ok((list_id, alias_id))
    }

    /// All lists owned by `owner`, newest first (joined with the alias row for
    /// the posting address).
    pub async fn list_lists_for_actor(&self, owner: &[u8; 32]) -> Result<Vec<MailListRecord>> {
        let owner = owner.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {LIST_SELECT_COLS}
                 FROM mail_lists ml
                 JOIN account_aliases a ON a.alias_id = ml.alias_id
                 WHERE ml.owner_actor_id = ?1
                 ORDER BY ml.created_at DESC, ml.rowid DESC"
            ))
            .context("prepare list_lists_for_actor")?;
        let rows = stmt
            .query_map(rusqlite::params![&owner[..]], row_to_list_record)
            .context("query list_lists_for_actor")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("read mail_lists row")?);
        }
        Ok(out)
    }

    /// One list scoped to its owner — the ownership gate the member sub-surface
    /// runs before any member read/write (`None` ⇒ not owned or absent ⇒ the
    /// handler returns `not_found`, never leaking another user's list).
    pub async fn get_list_for_owner(
        &self,
        list_id: &[u8; 16],
        owner: &[u8; 32],
    ) -> Result<Option<MailListRecord>> {
        let id = *list_id;
        let owner = owner.to_vec();
        let conn = self.conn.lock().await;
        let rec = conn
            .query_row(
                &format!(
                    "SELECT {LIST_SELECT_COLS}
                     FROM mail_lists ml
                     JOIN account_aliases a ON a.alias_id = ml.alias_id
                     WHERE ml.list_id = ?1 AND ml.owner_actor_id = ?2"
                ),
                rusqlite::params![&id[..], &owner[..]],
                row_to_list_record,
            )
            .optional()
            .context("query get_list_for_owner")?;
        Ok(rec)
    }

    /// Full-overwrite the editable metadata of a list the caller owns (the
    /// friendly-name / description / List-Help / List-Archive sources + the
    /// per-list `recipients_per_send` override; the posting address is the
    /// alias row and is immutable, like an alias `kind`). Owner-scoped:
    /// `Ok(false)` if no `(list_id, owner)` row matches.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_list_metadata(
        &self,
        list_id: &[u8; 16],
        owner: &[u8; 32],
        friendly_name: Option<&str>,
        description: Option<&str>,
        list_help_url: Option<&str>,
        list_archive_url: Option<&str>,
        recipients_per_send: Option<i64>,
    ) -> Result<bool> {
        let id = *list_id;
        let owner = owner.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE mail_lists
                 SET list_friendly_name = ?1, description = ?2,
                     list_help_url = ?3, list_archive_url = ?4,
                     recipients_per_send = ?5
                 WHERE list_id = ?6 AND owner_actor_id = ?7",
                rusqlite::params![
                    friendly_name,
                    description,
                    list_help_url,
                    list_archive_url,
                    recipients_per_send,
                    &id[..],
                    &owner[..],
                ],
            )
            .context("update_list_metadata")?;
        Ok(n > 0)
    }

    /// Destructively delete a list the caller owns. Deletes the `account_aliases`
    /// row, whose `ON DELETE CASCADE` chain removes the `mail_lists` row and all
    /// `mail_list_members` (§ The list as an alias row). Owner-scoped via the
    /// `mail_lists` row; `Ok(false)` if no `(list_id, owner)` match.
    pub async fn delete_list(&self, list_id: &[u8; 16], owner: &[u8; 32]) -> Result<bool> {
        let id = *list_id;
        let owner = owner.to_vec();
        let conn = self.conn.lock().await;
        // Resolve + ownership-check the alias_id in one shot, then delete the
        // alias (the cascade does the rest). Foreign-key enforcement is on
        // (the DB opens with `PRAGMA foreign_keys = ON`), so deleting the alias
        // row tears down the list + members.
        let alias_id: Option<Vec<u8>> = conn
            .query_row(
                "SELECT alias_id FROM mail_lists WHERE list_id = ?1 AND owner_actor_id = ?2",
                rusqlite::params![&id[..], &owner[..]],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .context("resolve alias_id for delete_list")?;
        let Some(alias_id) = alias_id else {
            return Ok(false);
        };
        let n = conn
            .execute(
                "DELETE FROM account_aliases WHERE alias_id = ?1",
                rusqlite::params![&alias_id[..]],
            )
            .context("delete list alias")?;
        Ok(n > 0)
    }

    // ── member sub-surface (ownership pre-verified by the handler) ──

    /// Members of a list, newest subscription first. `include_unsubscribed =
    /// false` returns only currently-subscribed members. The caller has already
    /// verified ownership via [`get_list_for_owner`].
    pub async fn list_members(
        &self,
        list_id: &[u8; 16],
        include_unsubscribed: bool,
    ) -> Result<Vec<MailListMemberRecord>> {
        let id = *list_id;
        let conn = self.conn.lock().await;
        let sql = if include_unsubscribed {
            "SELECT member_id, recipient_address, subscribed_at, unsubscribed_at
             FROM mail_list_members WHERE list_id = ?1
             ORDER BY subscribed_at DESC, rowid DESC"
        } else {
            "SELECT member_id, recipient_address, subscribed_at, unsubscribed_at
             FROM mail_list_members WHERE list_id = ?1 AND unsubscribed_at IS NULL
             ORDER BY subscribed_at DESC, rowid DESC"
        };
        let mut stmt = conn.prepare(sql).context("prepare list_members")?;
        let rows = stmt
            .query_map(rusqlite::params![&id[..]], |row| {
                Ok(MailListMemberRecord {
                    member_id: blob_col_to_array(row.get(0)?, 0, "member_id")?,
                    recipient_address: row.get(1)?,
                    subscribed_at: row.get(2)?,
                    unsubscribed_at: row.get(3)?,
                })
            })
            .context("query list_members")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("read mail_list_members row")?);
        }
        Ok(out)
    }

    /// `(subscribed_count, unsubscribed_count)` for a list — the
    /// `mail-list-members-summary` figures.
    pub async fn count_members(&self, list_id: &[u8; 16]) -> Result<(i64, i64)> {
        let id = *list_id;
        let conn = self.conn.lock().await;
        let (sub, unsub) = conn
            .query_row(
                "SELECT
                    COUNT(*) FILTER (WHERE unsubscribed_at IS NULL),
                    COUNT(*) FILTER (WHERE unsubscribed_at IS NOT NULL)
                 FROM mail_list_members WHERE list_id = ?1",
                rusqlite::params![&id[..]],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .context("count_members")?;
        Ok((sub, unsub))
    }

    /// Subscribe one canonical (lower-cased) address with its precomputed
    /// one-click token. Idempotent on the `UNIQUE(list_id, recipient_address)`
    /// key: a duplicate leaves the existing row untouched (subscription is
    /// sticky — re-subscribing an unsubscribed member is the explicit
    /// `resubscribe` path). Refreshes `member_count` on a fresh insert.
    pub async fn add_member(
        &self,
        list_id: &[u8; 16],
        recipient_address: &str,
        one_click_token: &str,
    ) -> Result<AddMemberOutcome> {
        let id = *list_id;
        let member_id = *Uuid::new_v4().as_bytes();
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin add_member")?;
        let inserted = tx
            .execute(
                "INSERT INTO mail_list_members
                    (member_id, list_id, recipient_address, subscribed_at,
                     unsubscribed_at, one_click_unsubscribe_token)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5)
                 ON CONFLICT(list_id, recipient_address) DO NOTHING",
                rusqlite::params![
                    &member_id[..],
                    &id[..],
                    recipient_address,
                    now,
                    one_click_token,
                ],
            )
            .context("insert mail_list_members")?;
        if inserted == 1 {
            refresh_member_count(&tx, &id).context("refresh member_count after add")?;
            tx.commit().context("commit add_member")?;
            return Ok(AddMemberOutcome::Added(member_id));
        }
        // Duplicate — read the existing member_id, leave the row as-is.
        let existing: Vec<u8> = tx
            .query_row(
                "SELECT member_id FROM mail_list_members
                 WHERE list_id = ?1 AND recipient_address = ?2",
                rusqlite::params![&id[..], recipient_address],
                |row| row.get(0),
            )
            .context("read existing member_id")?;
        tx.commit().context("commit add_member (dup)")?;
        Ok(AddMemberOutcome::AlreadyExists(blob_to_array(
            &existing,
            "member_id",
        )?))
    }

    /// Bulk-subscribe `(canonical_address, token)` pairs (the handler has
    /// already validated syntax + dropped local-domain / invalid addresses).
    /// Returns `(added, skipped_duplicate)`. One transaction; `member_count` is
    /// refreshed once at the end.
    pub async fn batch_add_members(
        &self,
        list_id: &[u8; 16],
        members: &[(String, String)],
    ) -> Result<(usize, usize)> {
        let id = *list_id;
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin batch_add_members")?;
        let mut added = 0usize;
        let mut skipped_duplicate = 0usize;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO mail_list_members
                        (member_id, list_id, recipient_address, subscribed_at,
                         unsubscribed_at, one_click_unsubscribe_token)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5)
                     ON CONFLICT(list_id, recipient_address) DO NOTHING",
                )
                .context("prepare batch insert")?;
            for (address, token) in members {
                let member_id = *Uuid::new_v4().as_bytes();
                let n = stmt
                    .execute(rusqlite::params![
                        &member_id[..],
                        &id[..],
                        address,
                        now,
                        token,
                    ])
                    .context("batch insert member")?;
                if n == 1 {
                    added += 1;
                } else {
                    skipped_duplicate += 1;
                }
            }
        }
        if added > 0 {
            refresh_member_count(&tx, &id).context("refresh member_count after batch")?;
        }
        tx.commit().context("commit batch_add_members")?;
        Ok((added, skipped_duplicate))
    }

    /// Owner-driven manual unsubscribe by address (the `mail-list-members`
    /// per-row button). Flips `unsubscribed_at` if currently subscribed;
    /// refreshes `member_count`. `Ok(false)` if no such subscribed member.
    pub async fn unsubscribe_member_by_address(
        &self,
        list_id: &[u8; 16],
        recipient_address: &str,
    ) -> Result<bool> {
        let id = *list_id;
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin unsubscribe by addr")?;
        let n = tx
            .execute(
                "UPDATE mail_list_members SET unsubscribed_at = ?1
                 WHERE list_id = ?2 AND recipient_address = ?3
                   AND unsubscribed_at IS NULL",
                rusqlite::params![now, &id[..], recipient_address],
            )
            .context("unsubscribe member by address")?;
        if n > 0 {
            refresh_member_count(&tx, &id).context("refresh member_count after unsub")?;
        }
        tx.commit().context("commit unsubscribe by addr")?;
        Ok(n > 0)
    }

    /// One-click unsubscribe by token (the HTTPS / mailto handlers — no owner
    /// scope; the token IS the auth, per RFC 8058 §3.3). Idempotent: a
    /// second click reports `AlreadyUnsubscribed`. Refreshes `member_count`
    /// (and `mail_lists.member_count` via the same path) on the transition.
    pub async fn unsubscribe_member_by_token(&self, token: &str) -> Result<UnsubscribeOutcome> {
        if token.is_empty() {
            return Ok(UnsubscribeOutcome::NotFound);
        }
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin unsubscribe by token")?;
        // Resolve the member + its list by the cached token index.
        let row: Option<(Vec<u8>, Option<i64>)> = tx
            .query_row(
                "SELECT list_id, unsubscribed_at FROM mail_list_members
                 WHERE one_click_unsubscribe_token = ?1
                 LIMIT 1",
                rusqlite::params![token],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()
            .context("resolve token")?;
        let Some((list_id_blob, unsubscribed_at)) = row else {
            tx.commit().ok();
            return Ok(UnsubscribeOutcome::NotFound);
        };
        if unsubscribed_at.is_some() {
            tx.commit().ok();
            return Ok(UnsubscribeOutcome::AlreadyUnsubscribed);
        }
        let list_id: [u8; 16] = blob_to_array(&list_id_blob, "list_id")?;
        tx.execute(
            "UPDATE mail_list_members SET unsubscribed_at = ?1
             WHERE one_click_unsubscribe_token = ?2 AND unsubscribed_at IS NULL",
            rusqlite::params![now, token],
        )
        .context("flip unsubscribed_at by token")?;
        refresh_member_count(&tx, &list_id).context("refresh member_count after token unsub")?;
        tx.commit().context("commit unsubscribe by token")?;
        Ok(UnsubscribeOutcome::Unsubscribed)
    }

    /// Owner-driven explicit re-subscribe by address (the `mail-list-members`
    /// per-row button — the only way back from a sticky unsubscribe). Flips
    /// `unsubscribed_at` back to `NULL`; refreshes `member_count`. `Ok(false)`
    /// if no such unsubscribed member.
    pub async fn resubscribe_member_by_address(
        &self,
        list_id: &[u8; 16],
        recipient_address: &str,
    ) -> Result<bool> {
        let id = *list_id;
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin resubscribe")?;
        let n = tx
            .execute(
                "UPDATE mail_list_members SET unsubscribed_at = NULL
                 WHERE list_id = ?1 AND recipient_address = ?2
                   AND unsubscribed_at IS NOT NULL",
                rusqlite::params![&id[..], recipient_address],
            )
            .context("resubscribe member")?;
        if n > 0 {
            refresh_member_count(&tx, &id).context("refresh member_count after resub")?;
        }
        tx.commit().context("commit resubscribe")?;
        Ok(n > 0)
    }

    /// Rotate the deployment-wide one-click-unsubscribe secret + re-tokenize
    /// every member under it (item #11; the Admin `rotate_list_unsubscribe_secret`
    /// RPC). Mirrors [`super::mail_srs::CacheDb::rotate_srs_secret`] but **prunes
    /// to a single row** — unlike SRS's 2-secret overlap window, rotation
    /// re-tokenizes, so no prior-secret verify window is needed (goal doc
    /// § Secret rotation: in-flight tokens are *invalidated*, not overlapped).
    /// Returns the number of members re-tokenized. Atomic: the secret swap +
    /// every token UPDATE commit together (a reader between rotation and commit
    /// sees the old secret + old tokens — consistent).
    ///
    /// One transaction over all members — fine for the rare admin-triggered
    /// rotation at realistic list sizes; batching is a future optimization for
    /// a deployment with very large lists.
    pub async fn rotate_list_unsubscribe_secret(&self) -> Result<usize> {
        use fauna_mail::lists::UnsubscribeTokenGenerator;
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| anyhow::anyhow!("getrandom failed: {e:?}"))?;
        let generator = UnsubscribeTokenGenerator::new(secret);
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin rotate secret")?;
        tx.execute("DELETE FROM mail_list_unsubscribe_secrets", [])
            .context("clear old unsubscribe secrets")?;
        tx.execute(
            "INSERT INTO mail_list_unsubscribe_secrets (secret, created_at) VALUES (?1, ?2)",
            rusqlite::params![secret.as_slice(), now],
        )
        .context("insert new unsubscribe secret")?;
        // Collect every member (the stored `recipient_address` is already
        // canonical, so re-deriving over it reproduces exactly what `add_member`
        // would mint under the new secret), then UPDATE each token.
        let members: Vec<([u8; 16], [u8; 16], String)> = {
            let mut stmt = tx
                .prepare("SELECT member_id, list_id, recipient_address FROM mail_list_members")
                .context("prepare retokenize scan")?;
            let rows = stmt
                .query_map([], |row| {
                    let m: [u8; 16] = blob_col_to_array(row.get(0)?, 0, "member_id")?;
                    let l: [u8; 16] = blob_col_to_array(row.get(1)?, 1, "list_id")?;
                    let a: String = row.get(2)?;
                    Ok((m, l, a))
                })
                .context("query members for retokenize")?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.context("read member for retokenize")?);
            }
            out
        };
        for (member_id, list_id, address) in &members {
            let token = generator.token_for(list_id, address);
            tx.execute(
                "UPDATE mail_list_members SET one_click_unsubscribe_token = ?1
                 WHERE member_id = ?2",
                rusqlite::params![token, &member_id[..]],
            )
            .context("update member token")?;
        }
        tx.commit().context("commit rotate secret")?;
        Ok(members.len())
    }

    // ── list-send pipeline (#10b / #6 / #7) ─────────────────────────

    /// The `mail_lists.list_id` whose posting address is `(local_domain,
    /// pattern)`, or `None`. Backs the #6 reject: an external MUA that AUTHs
    /// and submits with `MAIL FROM` = a list address is refused at
    /// `enqueue_outbound_mail` (per-recipient RFC 8058 stamping is impossible
    /// for a single-body SMTP submission — the canonical path is the explicit
    /// `send_list_message` RPC). `local_domain` + `pattern` are matched
    /// case-insensitively (alias rows are stored lower-cased).
    pub async fn lookup_list_id_for_address(
        &self,
        local_domain: &str,
        pattern: &str,
    ) -> Result<Option<[u8; 16]>> {
        let local_domain = local_domain.to_ascii_lowercase();
        let pattern = pattern.to_ascii_lowercase();
        let conn = self.conn.lock().await;
        let id: Option<Vec<u8>> = conn
            .query_row(
                "SELECT ml.list_id
                 FROM mail_lists ml
                 JOIN account_aliases a ON a.alias_id = ml.alias_id
                 WHERE a.kind = 'list' AND a.local_domain = ?1 AND a.pattern = ?2
                 LIMIT 1",
                rusqlite::params![&local_domain, &pattern],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .context("lookup_list_id_for_address")?;
        id.map(|b| blob_to_array(&b, "list_id")).transpose()
    }

    /// Currently-subscribed members of a list with their cached one-click
    /// tokens — the `send_list_message` fan-out input. Subscribed-only
    /// (`unsubscribed_at IS NULL`); a member with a NULL/empty token is skipped
    /// (unreachable for rows written by `add_member`/`batch_add_members`, which
    /// always derive it). The caller has already verified ownership.
    pub async fn list_subscribed_members_for_send(
        &self,
        list_id: &[u8; 16],
    ) -> Result<Vec<ListSendMember>> {
        let id = *list_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT recipient_address, one_click_unsubscribe_token
                 FROM mail_list_members
                 WHERE list_id = ?1 AND unsubscribed_at IS NULL
                 ORDER BY subscribed_at ASC, rowid ASC",
            )
            .context("prepare list_subscribed_members_for_send")?;
        let rows = stmt
            .query_map(rusqlite::params![&id[..]], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .context("query list_subscribed_members_for_send")?;
        let mut out = Vec::new();
        for r in rows {
            let (recipient_address, token) = r.context("read send member row")?;
            match token {
                Some(t) if !t.is_empty() => out.push(ListSendMember {
                    recipient_address,
                    one_click_unsubscribe_token: t,
                }),
                _ => tracing::warn!(
                    address = %recipient_address,
                    "list member has no cached unsubscribe token; skipped from send"
                ),
            }
        }
        Ok(out)
    }

    /// Atomically reserve quota for one list send of `recipient_count`
    /// recipients (§ Per-list rate accounting). Checks the three caps in
    /// order — per-send (hard), per-account-per-day, per-deployment-per-day
    /// (tempfail) — and, only when all pass, increments the per-list meters
    /// (`sends_today` / `recipients_today`, lazily zeroed on the first send of
    /// a new UTC day via `counters_day`), the per-account day counter, and the
    /// deployment day counter. `now_ms` is epoch-millis (the mail subsystem
    /// convention); the day bucket is `now_ms / 86_400_000`. The caller has
    /// verified ownership (`owner` is the list's `owner_actor_id`).
    pub async fn try_consume_list_quota(
        &self,
        list_id: &[u8; 16],
        owner: &[u8; 32],
        recipient_count: i64,
        caps: ListSendCaps,
        now_ms: i64,
    ) -> Result<ListQuotaOutcome> {
        let id = *list_id;
        let owner_v = owner.to_vec();
        let day = now_ms.div_euclid(86_400_000);
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin try_consume_list_quota")?;

        // 1) Per-send hard cap (§ The per-send cap — `552`, no auto-chunk).
        if recipient_count > caps.per_send {
            return Ok(ListQuotaOutcome::PerSendExceeded { cap: caps.per_send });
        }

        // 2) Per-account-per-day ceiling (§ The per-day per-account cap — `452`).
        let account_used: i64 = tx
            .query_row(
                "SELECT recipients_sent FROM mail_list_account_daily_counter
                 WHERE actor_id = ?1 AND day = ?2",
                rusqlite::params![&owner_v[..], day],
                |r| r.get(0),
            )
            .optional()
            .context("read account day counter")?
            .unwrap_or(0);
        if account_used + recipient_count > caps.per_account_per_day {
            return Ok(ListQuotaOutcome::PerAccountDayExceeded {
                cap: caps.per_account_per_day,
                remaining: (caps.per_account_per_day - account_used).max(0),
            });
        }

        // 3) Deployment-wide per-day ceiling (§ The per-day per-deployment cap).
        let deployment_used: i64 = tx
            .query_row(
                "SELECT recipients_sent FROM mail_list_deployment_daily_counter WHERE day = ?1",
                rusqlite::params![day],
                |r| r.get(0),
            )
            .optional()
            .context("read deployment day counter")?
            .unwrap_or(0);
        if deployment_used + recipient_count > caps.per_deployment_per_day {
            return Ok(ListQuotaOutcome::PerDeploymentDayExceeded {
                cap: caps.per_deployment_per_day,
                remaining: (caps.per_deployment_per_day - deployment_used).max(0),
            });
        }

        // All caps pass → reserve. Lazy-reset the per-list meters on a new day
        // (the displayed meter self-corrects on this first send of the day).
        let counters_day: i64 = tx
            .query_row(
                "SELECT counters_day FROM mail_lists WHERE list_id = ?1",
                rusqlite::params![&id[..]],
                |r| r.get(0),
            )
            .context("read counters_day")?;
        if counters_day != day {
            tx.execute(
                "UPDATE mail_lists
                 SET sends_today = 0, recipients_today = 0, counters_day = ?2
                 WHERE list_id = ?1",
                rusqlite::params![&id[..], day],
            )
            .context("lazy-reset per-list meters")?;
        }
        tx.execute(
            "UPDATE mail_lists
             SET sends_today = sends_today + 1,
                 recipients_today = recipients_today + ?2,
                 last_send_at = ?3
             WHERE list_id = ?1",
            rusqlite::params![&id[..], recipient_count, now_ms],
        )
        .context("bump per-list meters")?;
        tx.execute(
            "INSERT INTO mail_list_account_daily_counter (actor_id, day, recipients_sent)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id, day) DO UPDATE SET recipients_sent = recipients_sent + ?3",
            rusqlite::params![&owner_v[..], day, recipient_count],
        )
        .context("bump account day counter")?;
        tx.execute(
            "INSERT INTO mail_list_deployment_daily_counter (day, recipients_sent)
             VALUES (?1, ?2)
             ON CONFLICT(day) DO UPDATE SET recipients_sent = recipients_sent + ?2",
            rusqlite::params![day, recipient_count],
        )
        .context("bump deployment day counter")?;
        tx.commit().context("commit try_consume_list_quota")?;
        Ok(ListQuotaOutcome::Allowed {
            account_remaining: (caps.per_account_per_day - (account_used + recipient_count)).max(0),
            deployment_remaining: (caps.per_deployment_per_day
                - (deployment_used + recipient_count))
                .max(0),
        })
    }

    /// Record one list-send event in `mail_list_sends` (the audit
    /// `list_list_send_history` reads). `delivered_count` is the count the nest
    /// successfully queued (per-recipient MX-delivery tracking is a later
    /// track; queued == delivered for this surface today). Returns the send id.
    pub async fn record_list_send(
        &self,
        list_id: &[u8; 16],
        owner: &[u8; 32],
        sent_at: i64,
        recipient_count: i64,
        delivered_count: i64,
        unsubscribed_during_send: i64,
    ) -> Result<[u8; 16]> {
        let id = *list_id;
        let owner_v = owner.to_vec();
        let send_id = *Uuid::new_v4().as_bytes();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO mail_list_sends
                (send_id, list_id, owner_actor_id, sent_at, recipient_count,
                 delivered_count, unsubscribed_during_send)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                &send_id[..],
                &id[..],
                &owner_v[..],
                sent_at,
                recipient_count,
                delivered_count,
                unsubscribed_during_send,
            ],
        )
        .context("insert mail_list_sends")?;
        Ok(send_id)
    }

    /// The send history of a list, newest first, capped at `limit`. The caller
    /// has already verified ownership.
    pub async fn list_send_history(
        &self,
        list_id: &[u8; 16],
        limit: u32,
    ) -> Result<Vec<MailListSendRecord>> {
        let id = *list_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT send_id, sent_at, recipient_count, delivered_count,
                        unsubscribed_during_send
                 FROM mail_list_sends WHERE list_id = ?1
                 ORDER BY sent_at DESC, rowid DESC LIMIT ?2",
            )
            .context("prepare list_send_history")?;
        let rows = stmt
            .query_map(rusqlite::params![&id[..], limit], |row| {
                Ok(MailListSendRecord {
                    send_id: blob_col_to_array(row.get(0)?, 0, "send_id")?,
                    sent_at: row.get(1)?,
                    recipient_count: row.get(2)?,
                    delivered_count: row.get(3)?,
                    unsubscribed_during_send: row.get(4)?,
                })
            })
            .context("query list_send_history")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("read mail_list_sends row")?);
        }
        Ok(out)
    }
}

/// Map a rusqlite write error to [`ListWriteError`], recognizing the
/// `UNIQUE(local_domain, pattern, kind)` violation as `Conflict`.
fn map_list_write_err(e: rusqlite::Error) -> ListWriteError {
    use rusqlite::ErrorCode;
    if let rusqlite::Error::SqliteFailure(f, _) = &e
        && f.code == ErrorCode::ConstraintViolation
    {
        return ListWriteError::Conflict;
    }
    ListWriteError::Other(anyhow::Error::from(e))
}

/// Recompute the cached `mail_lists.member_count` (= subscribed members) for
/// one list from `mail_list_members`, inside the caller's transaction. Keeping
/// the cache exact after every membership change avoids incremental-drift bugs.
fn refresh_member_count(tx: &rusqlite::Transaction, list_id: &[u8; 16]) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE mail_lists SET member_count =
            (SELECT COUNT(*) FROM mail_list_members
             WHERE list_id = ?1 AND unsubscribed_at IS NULL)
         WHERE list_id = ?1",
        rusqlite::params![&list_id[..]],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;

    const OWNER_A: [u8; 32] = [0xA1u8; 32];
    const OWNER_B: [u8; 32] = [0xB2u8; 32];

    async fn db() -> CacheDb {
        CacheDb::open_in_memory().unwrap()
    }

    #[tokio::test]
    async fn auto_seeds_exactly_one_32_byte_secret() {
        let db = db().await;
        let secret = db
            .get_active_list_unsubscribe_secret()
            .await
            .unwrap()
            .expect("first open seeds the unsubscribe secret");
        assert_eq!(secret.len(), 32, "unsubscribe secret is 32 bytes");
    }

    #[tokio::test]
    async fn active_is_the_newest_row() {
        let db = db().await;
        let seed = db
            .get_active_list_unsubscribe_secret()
            .await
            .unwrap()
            .unwrap();
        let newer = vec![0xCDu8; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO mail_list_unsubscribe_secrets (secret, created_at) VALUES (?1, ?2)",
                rusqlite::params![newer.as_slice(), 9_999_999_999i64],
            )
            .unwrap();
        }
        let active = db
            .get_active_list_unsubscribe_secret()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active, newer, "active secret is the newest row");
        assert_ne!(active, seed);
    }

    #[tokio::test]
    async fn create_persists_alias_and_list_rows() {
        let db = db().await;
        let (list_id, alias_id) = db
            .create_list(
                &OWNER_A,
                "Fauna.Example",
                "Bob-Weekly",
                Some("Bob's Weekly"),
                Some("a newsletter"),
                None,
                None,
                Some(2000),
            )
            .await
            .unwrap();
        // The list is enumerable, with the lower-cased address from the alias.
        let lists = db.list_lists_for_actor(&OWNER_A).await.unwrap();
        assert_eq!(lists.len(), 1);
        let rec = &lists[0];
        assert_eq!(rec.list_id, list_id);
        assert_eq!(rec.alias_id, alias_id);
        assert_eq!(rec.local_domain, "fauna.example");
        assert_eq!(rec.pattern, "bob-weekly");
        assert_eq!(rec.friendly_name.as_deref(), Some("Bob's Weekly"));
        assert_eq!(rec.recipients_per_send, Some(2000));
        assert_eq!(rec.member_count, 0);
        // The matching `kind='list'` alias row exists.
        let conn = db.conn.lock().await;
        let kind: String = conn
            .query_row(
                "SELECT kind FROM account_aliases WHERE alias_id = ?1",
                rusqlite::params![&alias_id[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "list");
    }

    #[tokio::test]
    async fn create_collides_on_duplicate_address() {
        let db = db().await;
        db.create_list(
            &OWNER_A,
            "fauna.example",
            "news",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        // Same (domain, pattern) again — even by a different owner — collides.
        let err = db
            .create_list(
                &OWNER_B,
                "fauna.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ListWriteError::Conflict), "got {err:?}");
    }

    #[tokio::test]
    async fn list_is_owner_scoped() {
        let db = db().await;
        let (list_id, _) = db
            .create_list(
                &OWNER_A,
                "fauna.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(db.list_lists_for_actor(&OWNER_A).await.unwrap().len(), 1);
        assert_eq!(db.list_lists_for_actor(&OWNER_B).await.unwrap().len(), 0);
        // get_list_for_owner enforces ownership.
        assert!(
            db.get_list_for_owner(&list_id, &OWNER_A)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.get_list_for_owner(&list_id, &OWNER_B)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn update_metadata_owner_scoped() {
        let db = db().await;
        let (list_id, _) = db
            .create_list(
                &OWNER_A,
                "fauna.example",
                "news",
                Some("old"),
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(
            db.update_list_metadata(
                &list_id,
                &OWNER_A,
                Some("new name"),
                Some("desc"),
                Some("https://h"),
                Some("https://a"),
                Some(100),
            )
            .await
            .unwrap()
        );
        let rec = db
            .get_list_for_owner(&list_id, &OWNER_A)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.friendly_name.as_deref(), Some("new name"));
        assert_eq!(rec.list_archive_url.as_deref(), Some("https://a"));
        assert_eq!(rec.recipients_per_send, Some(100));
        // A non-owner can't update.
        assert!(
            !db.update_list_metadata(&list_id, &OWNER_B, Some("x"), None, None, None, None)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn delete_cascades_alias_list_and_members() {
        let db = db().await;
        let (list_id, alias_id) = db
            .create_list(
                &OWNER_A,
                "fauna.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        db.add_member(&list_id, "alice@example.com", "tokA")
            .await
            .unwrap();
        // A non-owner delete is a no-op.
        assert!(!db.delete_list(&list_id, &OWNER_B).await.unwrap());
        assert!(db.delete_list(&list_id, &OWNER_A).await.unwrap());
        // Alias, list, and member rows all gone.
        let conn = db.conn.lock().await;
        let alias_n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM account_aliases WHERE alias_id = ?1",
                rusqlite::params![&alias_id[..]],
                |r| r.get(0),
            )
            .unwrap();
        let list_n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mail_lists WHERE list_id = ?1",
                rusqlite::params![&list_id[..]],
                |r| r.get(0),
            )
            .unwrap();
        let member_n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mail_list_members WHERE list_id = ?1",
                rusqlite::params![&list_id[..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!((alias_n, list_n, member_n), (0, 0, 0));
    }

    #[tokio::test]
    async fn add_member_is_idempotent_and_tracks_count() {
        let db = db().await;
        let (list_id, _) = db
            .create_list(
                &OWNER_A,
                "fauna.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let first = db
            .add_member(&list_id, "alice@example.com", "tokA")
            .await
            .unwrap();
        assert!(matches!(first, AddMemberOutcome::Added(_)));
        // Duplicate address → AlreadyExists with the same member_id, count unchanged.
        let dup = db
            .add_member(&list_id, "alice@example.com", "tokA")
            .await
            .unwrap();
        assert!(matches!(dup, AddMemberOutcome::AlreadyExists(_)));
        assert_eq!(first.member_id(), dup.member_id());
        db.add_member(&list_id, "bob@example.com", "tokB")
            .await
            .unwrap();
        let rec = db
            .get_list_for_owner(&list_id, &OWNER_A)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.member_count, 2);
        let (sub, unsub) = db.count_members(&list_id).await.unwrap();
        assert_eq!((sub, unsub), (2, 0));
    }

    #[tokio::test]
    async fn unsubscribe_and_resubscribe_round_trip() {
        let db = db().await;
        let (list_id, _) = db
            .create_list(
                &OWNER_A,
                "fauna.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        db.add_member(&list_id, "alice@example.com", "tokA")
            .await
            .unwrap();
        db.add_member(&list_id, "bob@example.com", "tokB")
            .await
            .unwrap();
        // Manual unsubscribe by address.
        assert!(
            db.unsubscribe_member_by_address(&list_id, "alice@example.com")
                .await
                .unwrap()
        );
        // Idempotent — second call no-ops.
        assert!(
            !db.unsubscribe_member_by_address(&list_id, "alice@example.com")
                .await
                .unwrap()
        );
        let rec = db
            .get_list_for_owner(&list_id, &OWNER_A)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.member_count, 1, "alice no longer counts");
        let (sub, unsub) = db.count_members(&list_id).await.unwrap();
        assert_eq!((sub, unsub), (1, 1));
        // subscribed-only listing excludes alice; full listing includes her.
        assert_eq!(db.list_members(&list_id, false).await.unwrap().len(), 1);
        assert_eq!(db.list_members(&list_id, true).await.unwrap().len(), 2);
        // Explicit re-subscribe restores her.
        assert!(
            db.resubscribe_member_by_address(&list_id, "alice@example.com")
                .await
                .unwrap()
        );
        let rec = db
            .get_list_for_owner(&list_id, &OWNER_A)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rec.member_count, 2);
    }

    #[tokio::test]
    async fn one_click_unsubscribe_by_token() {
        let db = db().await;
        let (list_id, _) = db
            .create_list(
                &OWNER_A,
                "fauna.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        db.add_member(&list_id, "alice@example.com", "TOKEN-ALICE")
            .await
            .unwrap();
        // Unknown / empty token → NotFound.
        assert_eq!(
            db.unsubscribe_member_by_token("nope").await.unwrap(),
            UnsubscribeOutcome::NotFound
        );
        assert_eq!(
            db.unsubscribe_member_by_token("").await.unwrap(),
            UnsubscribeOutcome::NotFound
        );
        // First click unsubscribes; second is idempotent.
        assert_eq!(
            db.unsubscribe_member_by_token("TOKEN-ALICE").await.unwrap(),
            UnsubscribeOutcome::Unsubscribed
        );
        assert_eq!(
            db.unsubscribe_member_by_token("TOKEN-ALICE").await.unwrap(),
            UnsubscribeOutcome::AlreadyUnsubscribed
        );
        let (sub, unsub) = db.count_members(&list_id).await.unwrap();
        assert_eq!((sub, unsub), (0, 1));
    }

    #[tokio::test]
    async fn batch_import_counts_added_and_duplicates() {
        let db = db().await;
        let (list_id, _) = db
            .create_list(
                &OWNER_A,
                "fauna.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        db.add_member(&list_id, "alice@example.com", "tokA")
            .await
            .unwrap();
        let members = vec![
            ("alice@example.com".to_string(), "tokA".to_string()), // dup
            ("bob@example.com".to_string(), "tokB".to_string()),
            ("carol@example.com".to_string(), "tokC".to_string()),
            ("bob@example.com".to_string(), "tokB".to_string()), // dup within batch
        ];
        let (added, skipped) = db.batch_add_members(&list_id, &members).await.unwrap();
        assert_eq!((added, skipped), (2, 2));
        let (sub, _) = db.count_members(&list_id).await.unwrap();
        assert_eq!(sub, 3);
    }

    const DAY_MS: i64 = 86_400_000;

    fn caps(per_send: i64, per_account: i64, per_deployment: i64) -> ListSendCaps {
        ListSendCaps {
            per_send,
            per_account_per_day: per_account,
            per_deployment_per_day: per_deployment,
        }
    }

    async fn list_with_members(db: &CacheDb, n: usize) -> [u8; 16] {
        let (list_id, _) = db
            .create_list(
                &OWNER_A,
                "fauna.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        for i in 0..n {
            db.add_member(&list_id, &format!("m{i}@example.com"), &format!("tok{i}"))
                .await
                .unwrap();
        }
        list_id
    }

    #[tokio::test]
    async fn quota_per_send_hard_cap_touches_no_counters() {
        let db = db().await;
        let list_id = list_with_members(&db, 6).await;
        let out = db
            .try_consume_list_quota(&list_id, &OWNER_A, 6, caps(5, 1000, 1000), DAY_MS)
            .await
            .unwrap();
        assert_eq!(out, ListQuotaOutcome::PerSendExceeded { cap: 5 });
        // No meters moved.
        let rec = db
            .get_list_for_owner(&list_id, &OWNER_A)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((rec.sends_today, rec.recipients_today), (0, 0));
    }

    #[tokio::test]
    async fn quota_per_account_day_tempfails_after_ceiling() {
        let db = db().await;
        let list_id = list_with_members(&db, 1).await;
        // First send of 60 passes (account cap 100).
        assert!(matches!(
            db.try_consume_list_quota(&list_id, &OWNER_A, 60, caps(5000, 100, 1_000_000), DAY_MS)
                .await
                .unwrap(),
            ListQuotaOutcome::Allowed { .. }
        ));
        // Second 60 would hit 120 > 100 → tempfail, counter untouched.
        assert_eq!(
            db.try_consume_list_quota(&list_id, &OWNER_A, 60, caps(5000, 100, 1_000_000), DAY_MS)
                .await
                .unwrap(),
            ListQuotaOutcome::PerAccountDayExceeded {
                cap: 100,
                remaining: 40
            }
        );
        // A smaller 40 still fits the leftover headroom.
        assert!(matches!(
            db.try_consume_list_quota(&list_id, &OWNER_A, 40, caps(5000, 100, 1_000_000), DAY_MS)
                .await
                .unwrap(),
            ListQuotaOutcome::Allowed {
                account_remaining: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn quota_per_deployment_day_tempfails() {
        let db = db().await;
        let list_id = list_with_members(&db, 1).await;
        assert!(matches!(
            db.try_consume_list_quota(&list_id, &OWNER_A, 80, caps(5000, 1_000_000, 100), DAY_MS)
                .await
                .unwrap(),
            ListQuotaOutcome::Allowed { .. }
        ));
        assert_eq!(
            db.try_consume_list_quota(&list_id, &OWNER_A, 80, caps(5000, 1_000_000, 100), DAY_MS)
                .await
                .unwrap(),
            ListQuotaOutcome::PerDeploymentDayExceeded {
                cap: 100,
                remaining: 20
            }
        );
    }

    #[tokio::test]
    async fn quota_lazy_resets_per_list_meters_and_account_counter_on_new_day() {
        let db = db().await;
        let list_id = list_with_members(&db, 1).await;
        db.try_consume_list_quota(&list_id, &OWNER_A, 90, caps(5000, 100, 1_000_000), DAY_MS)
            .await
            .unwrap();
        let rec = db
            .get_list_for_owner(&list_id, &OWNER_A)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((rec.sends_today, rec.recipients_today), (1, 90));
        // Next UTC day: the per-list meters reset AND the account day counter
        // is a fresh key, so a 90 that would have tempfailed same-day passes.
        let next_day = DAY_MS * 2;
        assert!(matches!(
            db.try_consume_list_quota(&list_id, &OWNER_A, 90, caps(5000, 100, 1_000_000), next_day)
                .await
                .unwrap(),
            ListQuotaOutcome::Allowed { .. }
        ));
        let rec = db
            .get_list_for_owner(&list_id, &OWNER_A)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((rec.sends_today, rec.recipients_today), (1, 90));
    }

    #[tokio::test]
    async fn subscribed_members_for_send_carries_tokens_subscribed_only() {
        let db = db().await;
        let (list_id, _) = db
            .create_list(
                &OWNER_A,
                "fauna.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        db.add_member(&list_id, "alice@example.com", "TOK-A")
            .await
            .unwrap();
        db.add_member(&list_id, "bob@example.com", "TOK-B")
            .await
            .unwrap();
        db.unsubscribe_member_by_address(&list_id, "bob@example.com")
            .await
            .unwrap();
        let members = db.list_subscribed_members_for_send(&list_id).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].recipient_address, "alice@example.com");
        assert_eq!(members[0].one_click_unsubscribe_token, "TOK-A");
    }

    #[tokio::test]
    async fn lookup_list_id_for_address_matches_case_insensitively() {
        let db = db().await;
        let (list_id, _) = db
            .create_list(
                &OWNER_A,
                "Fauna.Example",
                "News",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            db.lookup_list_id_for_address("fauna.example", "news")
                .await
                .unwrap(),
            Some(list_id)
        );
        assert_eq!(
            db.lookup_list_id_for_address("fauna.example", "not-a-list")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn record_and_read_send_history_newest_first() {
        let db = db().await;
        let list_id = list_with_members(&db, 1).await;
        db.record_list_send(&list_id, &OWNER_A, 1000, 5, 5, 0)
            .await
            .unwrap();
        db.record_list_send(&list_id, &OWNER_A, 2000, 9, 9, 1)
            .await
            .unwrap();
        let hist = db.list_send_history(&list_id, 10).await.unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].sent_at, 2000);
        assert_eq!(hist[0].recipient_count, 9);
        assert_eq!(hist[0].unsubscribed_during_send, 1);
        assert_eq!(hist[1].sent_at, 1000);
        // limit honored.
        assert_eq!(db.list_send_history(&list_id, 1).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rotate_secret_retokenizes_and_invalidates_old_tokens() {
        use fauna_mail::lists::UnsubscribeTokenGenerator;
        let db = db().await;
        let (list_id, _) = db
            .create_list(
                &OWNER_A,
                "fauna.example",
                "news",
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        // Subscribe a member with the token derived from the *current* secret —
        // exactly what the add_list_member handler does.
        let secret0 = db
            .get_active_list_unsubscribe_secret()
            .await
            .unwrap()
            .unwrap();
        let gen0 = UnsubscribeTokenGenerator::new(secret0[..32].try_into().unwrap());
        let token_old = gen0.token_for(&list_id, "alice@example.com");
        db.add_member(&list_id, "alice@example.com", &token_old)
            .await
            .unwrap();

        // Rotate → re-tokenizes the one member, replaces the secret.
        assert_eq!(db.rotate_list_unsubscribe_secret().await.unwrap(), 1);
        let secret1 = db
            .get_active_list_unsubscribe_secret()
            .await
            .unwrap()
            .unwrap();
        assert_ne!(secret1, secret0, "rotation replaces the secret");
        let gen1 = UnsubscribeTokenGenerator::new(secret1[..32].try_into().unwrap());
        let token_new = gen1.token_for(&list_id, "alice@example.com");
        assert_ne!(token_new, token_old, "the member's token changed");

        // The old token (from an in-flight message) no longer resolves; the new
        // one does.
        assert_eq!(
            db.unsubscribe_member_by_token(&token_old).await.unwrap(),
            UnsubscribeOutcome::NotFound,
            "in-flight old token is invalidated"
        );
        assert_eq!(
            db.unsubscribe_member_by_token(&token_new).await.unwrap(),
            UnsubscribeOutcome::Unsubscribed,
            "the re-tokenized value resolves"
        );

        // Exactly one secret row remains (no overlap window).
        let conn = db.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mail_list_unsubscribe_secrets",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "rotation prunes to a single secret row");
    }
}
