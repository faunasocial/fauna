//! Family safety v1 — the guardianship link + per-ward reach policy
//! (`docs/goal/behavior/family-safety.md` § Wire & data shape;
//! tracked internally).
//!
//! The link row's existence IS the supervised designation (no separate flag —
//! "supervised with no guardian" is unrepresentable). Rows are created only
//! inside the admission transaction (`admin::create_user_with_handle` with a
//! guardian) and removed only by graduation / ward deletion, so every read
//! here sees a complete link + policy pair.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use fauna_core::data::DmPeerVerdict;
use fauna_core::obligation::{ContentFloor, ContentPolicy};
use fauna_core::screen_time::ScreenTimePolicy;

use super::{CacheDb, now_epoch_secs};

/// How long a ward's sent Message-ID stays correlatable — the window inside
/// which a remote MTA's bounce of that message is delivered rather than held
/// (`family-safety.md` § The mail gate).
///
/// 30 days, comfortably past the point any real MTA is still retrying: RFC 5321
/// § 4.5.4.1 puts the give-up point at 4–5 days, and this deployment's own
/// default (`permanent_failure_timeout_hours = 120`) is 5. A bounce arriving
/// after the window is **held**, not lost — the guardian releases it. Erring
/// long costs a few dozen bytes per message the ward sent; erring short costs a
/// guardian a spurious review.
const SENT_MSGID_RETENTION_SECS: i64 = 30 * 86_400;

/// How long a pending transfer proposal awaits the proposed guardian's answer
/// before lapsing (`family-safety.md` § Graduation & transfer). Expiry is
/// lazy: an expired row is invisible to reads and un-acceptable, and is pruned
/// opportunistically on the next proposal write — no background job. The
/// initiator can always re-initiate, so erring short costs one re-send; erring
/// long leaves a stale duty offer dangling in someone's prompt.
const PENDING_TRANSFER_TTL_SECS: i64 = 7 * 86_400;

/// A pending child-initiated contact ask expires lazily after 30 days
/// (`family-safety.md` § Child-initiated contact requests: between transfer's
/// 7 and the knock TTL's 90 — an ask survives a guardian's vacation week and
/// does not outlive a season).
const PENDING_CONTACT_REQUEST_TTL_SECS: i64 = 30 * 86_400;

/// Pending-ask cap per ward — a conforming-client bug (or a determined child)
/// can never flood the guardian's queue (hard-coded, no-operator bucket 1).
pub const MAX_PENDING_CONTACT_REQUESTS: i64 = 32;

/// A pending feed-source ask expires lazily after 30 days (`family-safety.md`
/// § Feed-source approvals) — the contact-ask window, for the same reason.
const PENDING_FEED_REQUEST_TTL_SECS: i64 = 30 * 86_400;

/// An **approved** feed-source ask — a grant — lapses 7 days after approval
/// (`family-safety.md` § Feed-source approvals: *"a grant is a fresh guardian
/// decision; the redeeming retry is prompt"*). Deliberately far shorter than the
/// 30-day pending window: a pending ask is only a question, but a live grant is
/// standing permission to reach outside the ward's policy.
const FEED_GRANT_TTL_SECS: i64 = 7 * 86_400;

/// Feed-source ask cap per ward — the [`MAX_PENDING_CONTACT_REQUESTS`] rule for
/// the other v1.x queue kind. Counts asks *and* unredeemed grants: both are open
/// rows the ward can create at will.
pub const MAX_PENDING_FEED_REQUESTS: i64 = 32;

/// The ceiling on a stored per-(ward, day, category) Guardian Notify count
/// (`family-safety.md` § Guardian Notify). The count is coarse "policy is
/// acting" telemetry, not an audited number, so a generous cap that prevents an
/// absurd or overflowing value is all it needs; a real day never approaches it.
/// The per-report delta is also clamped in the handler before it reaches here.
const CONTENT_NOTICE_COUNT_CAP: i64 = 1_000_000;

/// The ceiling on a stored per-(ward, day) screen-time usage total
/// (`family-safety.md` § Screen time). Like [`CONTENT_NOTICE_COUNT_CAP`] this
/// is coarse telemetry, not an audited number — the cap only stops an absurd
/// or overflowing value (a real day across many devices stays in the low
/// thousands); the per-report delta is also clamped in the handler.
const GUARDIAN_USAGE_MINUTES_CAP: i64 = 1_000_000;

/// One guardianship edge. v1 admits exactly one guardian per supervised
/// account; the composite-PK shape keeps co-guardians representable later.
#[derive(Debug, Clone, PartialEq)]
pub struct GuardianshipRow {
    pub supervised_actor_id: Vec<u8>,
    pub guardian_actor_id: Vec<u8>,
    pub created_at: i64,
    /// The ward's last-reported clamped UTC offset in minutes (v22,
    /// `family-safety.md` § Screen time — the day-bucket rule): what `status`
    /// uses to derive "the ward's local today" for its readouts. `0` = UTC
    /// (the default, and the pre-offset behaviour).
    pub ward_utc_offset_minutes: i32,
}

/// The per-ward reach-policy document (`family-safety.md` § Guardian policy
/// pillar 1). Defaults are the unsupervised-equivalent values — a fresh link
/// changes nothing until the guardian tightens it.
#[derive(Debug, Clone, PartialEq)]
pub struct GuardianPolicyRow {
    pub supervised_actor_id: Vec<u8>,
    pub contact_approval: bool,
    pub unknown_sender_mail: String,
    pub federation_contact: bool,
    pub feed_sources: String,
    pub updated_at: i64,
    /// v1.x content pillar (`family-safety.md` § Content policy): the raw
    /// per-category floor strings (`inherit` | `collapse` | `block`) as stored.
    /// `policy_row_to_wire` folds these into a `ContentPolicy`, or `None` when
    /// all four are the default `inherit` (the unsupervised-equivalent).
    pub content_nsfw: String,
    pub content_spam: String,
    pub content_phishing: String,
    pub content_commercial: String,
    /// v1.x Guardian Notify knob (`family-safety.md` § Guardian Notify): when on,
    /// the ward's client reports coarse per-category enforcement counts and the
    /// guardian is notified. Default off (the unsupervised-equivalent).
    pub content_notify: bool,
    /// v1.x screen-time pillar (`family-safety.md` § Screen time): usage window
    /// (minutes from local midnight, wrap-capable) + daily budget. `None` = that
    /// control is unset.
    pub screen_window_start: Option<u16>,
    pub screen_window_end: Option<u16>,
    pub screen_daily_minutes: Option<u16>,
    /// The bridge-DM gate's knob (v25, `family-safety.md` § The bridge-DM gate)
    /// — `allow` | `hold`, stored verbatim. Parsed at the point of use via
    /// `fauna_core::data::UnknownPeerDm::from_wire`, which is where the
    /// fail-closed rule lives: a value only a newer nest could write must never
    /// resolve permissively here.
    pub unknown_peer_dm: String,
    /// The guardian tier's controversial-class feature sub-document
    /// (`dynamic-features.md` § Wire & data shape), as stored: the DAG-CBOR map
    /// of stable-feature-key -> `FeaturePolicy`, or `None` when this guardian has
    /// expressed no feature opinion (the unsupervised-equivalent).
    ///
    /// Kept as raw bytes on the row rather than decoded here for the same reason
    /// `unknown_peer_dm` is kept a `String`: a document a *newer* nest wrote must
    /// round-trip through this one untouched, so the parse belongs at the point
    /// of use.
    pub features_document: Option<Vec<u8>>,
}

impl GuardianPolicyRow {
    /// Fold the stored content-floor columns into a [`ContentPolicy`], or `None`
    /// when all four are the default `inherit` (the unsupervised-equivalent — no
    /// floor set). Returning `None` for the default lets the wire omit the pillar
    /// (`skip_serializing_if`), so an all-default read round-trips to the shape a
    /// v1 client sent (`family-safety.md` § Policy-update compatibility).
    pub fn content_policy(&self) -> Option<ContentPolicy> {
        let cp = ContentPolicy {
            nsfw: ContentFloor::from_wire(&self.content_nsfw),
            spam: ContentFloor::from_wire(&self.content_spam),
            phishing: ContentFloor::from_wire(&self.content_phishing),
            commercial: ContentFloor::from_wire(&self.content_commercial),
        };
        (cp != ContentPolicy::default()).then_some(cp)
    }

    /// Fold the stored screen-time columns into a [`ScreenTimePolicy`], or `None`
    /// when every control is unset (the unsupervised-equivalent).
    pub fn screen_time(&self) -> Option<ScreenTimePolicy> {
        let st = ScreenTimePolicy {
            window_start: self.screen_window_start,
            window_end: self.screen_window_end,
            daily_minutes: self.screen_daily_minutes,
        };
        (!st.is_unset()).then_some(st)
    }
}

/// One pending child-initiated contact ask (`family-safety.md`
/// § Child-initiated contact requests). At most one exists per (ward, peer)
/// — the table's PK; approve mints the accepted edge, deny drops the row.
#[derive(Debug, Clone, PartialEq)]
pub struct ContactRequestRow {
    pub supervised_actor_id: Vec<u8>,
    pub peer_actor_id: Vec<u8>,
    pub created_at: i64,
}

/// The outcome of recording a ward's contact ask — the handler maps each arm
/// to its reply (`family-safety.md` § Child-initiated contact requests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactRequestAdd {
    /// A new pending row was created — ring the guardian's doorbell. Carries
    /// the row's AUTOINCREMENT id: the doorbell dedup key must be unique per
    /// *created row*, and neither the creation timestamp (a deny + re-ask
    /// inside one epoch second collide) nor a plain rowid (SQLite reuses the
    /// max rowid after a delete) is.
    Created { row_id: i64 },
    /// The same ask is already pending — a quiet no-op, never a re-ring.
    AlreadyPending,
    /// The ward's pending-ask cap is reached — refuse.
    CapExceeded,
    /// The caller is not supervised (raced a graduation) — nothing recorded.
    NotSupervised,
}

/// One live feed-source ask (`family-safety.md` § Feed-source approvals): a
/// pending question while `approved_at` is `None`, a single-use grant once the
/// guardian sets it. At most one exists per (ward, bridge, operation, target) —
/// the table's UNIQUE key, and exactly the key the redeeming gate matches on.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedRequestRow {
    pub supervised_actor_id: Vec<u8>,
    pub bridge_id: String,
    pub operation: String,
    pub target: String,
    /// Display-only (the ward's petname / feed name) — never authorizing.
    pub label: String,
    pub created_at: i64,
    /// `None` = pending; `Some` = the instant the guardian granted it.
    pub approved_at: Option<i64>,
}

impl FeedRequestRow {
    /// Whether this row is a grant awaiting redemption rather than an ask
    /// awaiting the guardian. Only live rows are ever read out, so this needs no
    /// window check of its own — the queries own the windows.
    pub fn is_granted(&self) -> bool {
        self.approved_at.is_some()
    }
}

/// The outcome of recording a ward's feed-source ask — the [`ContactRequestAdd`]
/// shape for the other v1.x queue kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedRequestAdd {
    /// A new pending row was created — ring the guardian's doorbell. Carries the
    /// AUTOINCREMENT id for the same reason [`ContactRequestAdd::Created`] does.
    Created { row_id: i64 },
    /// An ask for this exact object is already open — pending *or* already
    /// granted and awaiting the ward's retry. A quiet no-op either way, never a
    /// re-ring: the guardian has already been asked, and a live grant means the
    /// ward's next move is to redeem it, not to ask again.
    AlreadyOpen,
    /// The ward's open-ask cap is reached — refuse.
    CapExceeded,
    /// The caller is not supervised (raced a graduation) — nothing recorded.
    NotSupervised,
}

/// One pending transfer proposal (`family-safety.md` § Graduation & transfer
/// — the consent handshake). At most one exists per ward (the table's PK);
/// the link is untouched until the proposed guardian accepts.
#[derive(Debug, Clone, PartialEq)]
pub struct GuardianTransferRow {
    pub supervised_actor_id: Vec<u8>,
    pub proposed_guardian_actor_id: Vec<u8>,
    /// Who proposed — the current guardian or the admin. Recorded for audit
    /// symmetry; authorization never keys on it (cancel re-checks
    /// guardian-or-admin live).
    pub initiated_by: Vec<u8>,
    pub created_at: i64,
}

/// The envelope sidecar for one held message (`family-safety.md` § Wire & data
/// shape). **Not** a hold store: the message's presence in the ward's held
/// mailbox is the hold. This row carries only what the guardian's queue renders
/// and the approve path allowlists — never anything derived from content.
#[derive(Debug, Clone, PartialEq)]
pub struct MailHoldRow {
    pub message_id: Vec<u8>,
    pub supervised_actor_id: Vec<u8>,
    /// Normalized envelope sender (`normalize_mail_address`), so the approve
    /// path's allowlist insert matches this row exactly.
    pub sender_address: String,
    pub created_at: i64,
}

impl CacheDb {
    /// The guardian of `supervised`, if the account is supervised.
    pub async fn get_guardian_of(&self, supervised: &[u8]) -> Result<Option<GuardianshipRow>> {
        let supervised = supervised.to_vec();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT supervised_actor_id, guardian_actor_id, created_at,
                    ward_utc_offset_minutes
             FROM guardianships WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
            |row| {
                Ok(GuardianshipRow {
                    supervised_actor_id: row.get(0)?,
                    guardian_actor_id: row.get(1)?,
                    created_at: row.get(2)?,
                    ward_utc_offset_minutes: row.get(3)?,
                })
            },
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get guardian of"),
        }
    }

    /// Every supervised account `guardian` guards (guardian-side read).
    pub async fn list_wards(&self, guardian: &[u8]) -> Result<Vec<GuardianshipRow>> {
        let guardian = guardian.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT supervised_actor_id, guardian_actor_id, created_at,
                        ward_utc_offset_minutes
                 FROM guardianships WHERE guardian_actor_id = ?1 ORDER BY created_at",
            )
            .context("prepare list wards")?;
        let rows = stmt
            .query_map(rusqlite::params![guardian], |row| {
                Ok(GuardianshipRow {
                    supervised_actor_id: row.get(0)?,
                    guardian_actor_id: row.get(1)?,
                    created_at: row.get(2)?,
                    ward_utc_offset_minutes: row.get(3)?,
                })
            })
            .context("query wards")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read ward row")?);
        }
        Ok(results)
    }

    /// The ward's reach-policy document.
    pub async fn get_guardian_policy(
        &self,
        supervised: &[u8],
    ) -> Result<Option<GuardianPolicyRow>> {
        let supervised = supervised.to_vec();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT supervised_actor_id, contact_approval, unknown_sender_mail,
                    federation_contact, feed_sources, updated_at,
                    content_nsfw, content_spam, content_phishing, content_commercial,
                    content_notify,
                    screen_window_start, screen_window_end, screen_daily_minutes,
                    unknown_peer_dm, features_document
             FROM guardian_policies WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
            |row| {
                Ok(GuardianPolicyRow {
                    supervised_actor_id: row.get(0)?,
                    contact_approval: row.get::<_, i64>(1)? != 0,
                    unknown_sender_mail: row.get(2)?,
                    federation_contact: row.get::<_, i64>(3)? != 0,
                    feed_sources: row.get(4)?,
                    updated_at: row.get(5)?,
                    content_nsfw: row.get(6)?,
                    content_spam: row.get(7)?,
                    content_phishing: row.get(8)?,
                    content_commercial: row.get(9)?,
                    content_notify: row.get::<_, i64>(10)? != 0,
                    screen_window_start: row.get::<_, Option<i64>>(11)?.map(|v| v as u16),
                    screen_window_end: row.get::<_, Option<i64>>(12)?.map(|v| v as u16),
                    screen_daily_minutes: row.get::<_, Option<i64>>(13)?.map(|v| v as u16),
                    unknown_peer_dm: row.get(14)?,
                    features_document: row.get(15)?,
                })
            },
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get guardian policy"),
        }
    }

    /// Guardian-validity read for admission-time designation
    /// (`family-safety.md` § The guardianship link): the named guardian must
    /// be an existing, non-suspended user that is not itself supervised.
    /// Returns a static reason on failure so handlers map it to a typed error.
    pub async fn check_guardian_admissible(
        &self,
        guardian: &[u8],
    ) -> Result<Result<(), &'static str>> {
        let g = guardian.to_vec();
        let conn = self.conn.lock().await;
        let user = conn.query_row(
            "SELECT suspended FROM users WHERE actor_id = ?1",
            rusqlite::params![g],
            |row| row.get::<_, i64>(0),
        );
        let suspended = match user {
            Ok(s) => s != 0,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(Err("not_found")),
            Err(e) => return Err(e).context("check guardian user"),
        };
        if suspended {
            return Ok(Err("suspended"));
        }
        let is_supervised: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM guardianships WHERE supervised_actor_id = ?1",
                rusqlite::params![g],
                |row| row.get(0),
            )
            .context("check guardian supervised")?;
        if is_supervised > 0 {
            // No chains: a supervised account cannot itself guard.
            return Ok(Err("supervised"));
        }
        Ok(Ok(()))
    }

    /// Update the ward's reach-policy document (`fauna.family.policy.update`).
    /// The caller has already verified guardian-ship and validated the values.
    /// Returns false when no policy row exists (not a supervised account).
    ///
    /// The four v1 reach knobs **replace** (their shipped wire semantics). Each
    /// v1.x pillar is `Option`: a present sub-document replaces its columns, an
    /// **absent** one leaves them unchanged (`family-safety.md` § Wire & data
    /// shape → *Policy-update compatibility*:97) — so a v1-era client saving
    /// reach knobs never clobbers a content/screen rule it cannot render. The
    /// whole update is one statement (one atomic write; no partial-pillar
    /// state is reachable on a crash).
    pub async fn update_guardian_policy(
        &self,
        supervised: &[u8],
        contact_approval: bool,
        unknown_sender_mail: &str,
        federation_contact: bool,
        feed_sources: &str,
        content_policy: Option<&ContentPolicy>,
        screen_time: Option<&ScreenTimePolicy>,
        content_notify: Option<bool>,
        unknown_peer_dm: Option<&str>,
        features: Option<&[u8]>,
    ) -> Result<bool> {
        use rusqlite::ToSql;

        let now = now_epoch_secs();
        // Build the dynamic statement + its params BEFORE awaiting the lock: the
        // `Box<dyn ToSql>` params are not `Send`, so nothing here may live across
        // an `.await` (the handler future must stay `Send`). Acquire the lock,
        // then build and execute synchronously.
        let conn = self.conn.lock().await;

        let mut sets = String::from(
            "contact_approval = ?, unknown_sender_mail = ?, federation_contact = ?, \
             feed_sources = ?, updated_at = ?",
        );
        let mut params: Vec<Box<dyn ToSql>> = vec![
            Box::new(contact_approval as i64),
            Box::new(unknown_sender_mail.to_string()),
            Box::new(federation_contact as i64),
            Box::new(feed_sources.to_string()),
            Box::new(now),
        ];
        if let Some(cp) = content_policy {
            sets.push_str(
                ", content_nsfw = ?, content_spam = ?, content_phishing = ?, \
                 content_commercial = ?",
            );
            params.push(Box::new(cp.nsfw.as_str().to_string()));
            params.push(Box::new(cp.spam.as_str().to_string()));
            params.push(Box::new(cp.phishing.as_str().to_string()));
            params.push(Box::new(cp.commercial.as_str().to_string()));
        }
        if let Some(st) = screen_time {
            sets.push_str(
                ", screen_window_start = ?, screen_window_end = ?, screen_daily_minutes = ?",
            );
            params.push(Box::new(st.window_start.map(i64::from)));
            params.push(Box::new(st.window_end.map(i64::from)));
            params.push(Box::new(st.daily_minutes.map(i64::from)));
        }
        if let Some(notify) = content_notify {
            sets.push_str(", content_notify = ?");
            params.push(Box::new(notify as i64));
        }
        // Absent-means-unchanged, like the pillars above — a v1-era client saving
        // the four reach knobs must not silently relax a bridge-DM gate it cannot
        // render. (This writer deliberately does NOT validate the value; the
        // handler's `validate_policy` does, which is what lets a test store the
        // unnameable value only a newer nest could write and pin the read path's
        // fail-closed parse.)
        if let Some(dm) = unknown_peer_dm {
            sets.push_str(", unknown_peer_dm = ?");
            params.push(Box::new(dm.to_string()));
        }
        // The guardian tier's feature sub-document (`dynamic-features.md`
        // § Wire & data shape), same absent-means-unchanged rule as the pillars
        // above — and for a sharper reason here than for any of them: this tier
        // is NEST-enforced, so a client that silently cleared it would not merely
        // fail to render a restriction, it would *lift* one.
        //
        // An empty map is a legitimate present value meaning "the guardian
        // withdrew every feature limit", stored as an empty document rather than
        // NULL so it stays distinguishable from "never spoke" for anything that
        // later wants to tell those apart.
        if let Some(features) = features {
            sets.push_str(", features_document = ?");
            params.push(Box::new(features.to_vec()));
        }
        // The WHERE placeholder binds last, matching its position in the SQL.
        params.push(Box::new(supervised.to_vec()));
        let sql = format!("UPDATE guardian_policies SET {sets} WHERE supervised_actor_id = ?");

        let n = conn
            .execute(
                &sql,
                rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            )
            .context("update guardian policy")?;
        Ok(n > 0)
    }

    /// Accumulate a coarse per-(ward, day, category) enforcement count for
    /// Guardian Notify (`family-safety.md` § Guardian Notify). `delta` is the
    /// increment the ward's client reported; the stored total is capped at
    /// [`CONTENT_NOTICE_COUNT_CAP`] so no client can drive it absurd. Carries
    /// **no content identifier** — the whole point of Notify.
    ///
    /// Like every family side table, a **no-op for an unsupervised account** (the
    /// `WHERE EXISTS` guardianship guard), so the handler's supervised-caller
    /// check is defense-in-depth rather than the only guard, and no adult's
    /// account ever accrues a row.
    pub async fn upsert_content_notice(
        &self,
        ward: &[u8],
        day: i64,
        category: &str,
        delta: u32,
    ) -> Result<()> {
        let w = ward.to_vec();
        let category = category.to_string();
        let delta = i64::from(delta).min(CONTENT_NOTICE_COUNT_CAP);
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO guardian_content_notices (supervised_actor_id, day, category, count)
             SELECT ?1, ?2, ?3, ?4
             WHERE EXISTS (SELECT 1 FROM guardianships WHERE supervised_actor_id = ?1)
             ON CONFLICT (supervised_actor_id, day, category)
             DO UPDATE SET count = MIN(count + excluded.count, ?5)",
            rusqlite::params![w, day, category, delta, CONTENT_NOTICE_COUNT_CAP],
        )
        .context("upsert content notice")?;
        Ok(())
    }

    /// The ward's coarse per-category enforcement counts for `day`
    /// (`family-safety.md` § Guardian Notify — the guardian's Family-surface
    /// readout). Ordered by category for a stable render; a zero count is
    /// omitted. Carries no content, only `(category, running total)`.
    pub async fn list_content_notices_for_day(
        &self,
        ward: &[u8],
        day: i64,
    ) -> Result<Vec<(String, u32)>> {
        let w = ward.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT category, count FROM guardian_content_notices
                 WHERE supervised_actor_id = ?1 AND day = ?2 AND count > 0
                 ORDER BY category",
            )
            .context("prepare list content notices")?;
        let rows = stmt
            .query_map(rusqlite::params![w, day], |row| {
                let category: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((category, count.max(0) as u32))
            })
            .context("query content notices")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read content notice row")?);
        }
        Ok(out)
    }

    /// Accumulate coarse foreground minutes into the ward's per-(local-)day
    /// cross-device total and return the day's total after the write
    /// (`family-safety.md` § Screen time — the reply carries the number the
    /// enforcing client locks on). A zero `delta` is a pure read. The stored
    /// total is capped at [`GUARDIAN_USAGE_MINUTES_CAP`]; the per-report delta
    /// is also clamped in the handler.
    ///
    /// Like every family side table, a **no-op for an unsupervised account**
    /// (the `WHERE EXISTS` guardianship guard — defense-in-depth behind the
    /// handler's link check), so no adult's account ever accrues a row; the
    /// read-back then honestly returns 0.
    pub async fn upsert_guardian_usage(&self, ward: &[u8], day: i64, delta: u32) -> Result<u32> {
        let w = ward.to_vec();
        let delta = i64::from(delta).min(GUARDIAN_USAGE_MINUTES_CAP);
        let conn = self.conn.lock().await;
        if delta > 0 {
            conn.execute(
                "INSERT INTO guardian_usage (supervised_actor_id, day, minutes)
                 SELECT ?1, ?2, ?3
                 WHERE EXISTS (SELECT 1 FROM guardianships WHERE supervised_actor_id = ?1)
                 ON CONFLICT (supervised_actor_id, day)
                 DO UPDATE SET minutes = MIN(minutes + excluded.minutes, ?4)",
                rusqlite::params![w, day, delta, GUARDIAN_USAGE_MINUTES_CAP],
            )
            .context("upsert guardian usage")?;
        }
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(
                    (SELECT minutes FROM guardian_usage
                     WHERE supervised_actor_id = ?1 AND day = ?2), 0)",
                rusqlite::params![w, day],
                |row| row.get(0),
            )
            .context("read guardian usage total")?;
        Ok(total.clamp(0, GUARDIAN_USAGE_MINUTES_CAP) as u32)
    }

    /// The ward's cross-device foreground total for `day` (0 when nothing was
    /// reported) — the `status` readout both roles render.
    pub async fn get_guardian_usage(&self, ward: &[u8], day: i64) -> Result<u32> {
        let w = ward.to_vec();
        let conn = self.conn.lock().await;
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(
                    (SELECT minutes FROM guardian_usage
                     WHERE supervised_actor_id = ?1 AND day = ?2), 0)",
                rusqlite::params![w, day],
                |row| row.get(0),
            )
            .context("read guardian usage")?;
        Ok(total.clamp(0, GUARDIAN_USAGE_MINUTES_CAP) as u32)
    }

    /// Record the ward's last-reported clamped UTC offset on the guardianship
    /// link (v22, `family-safety.md` § Screen time — the day-bucket rule).
    /// Both report handlers call this; `status` reads it to derive "the
    /// ward's local today". A no-op for an unsupervised account.
    pub async fn set_ward_utc_offset(&self, ward: &[u8], offset_minutes: i32) -> Result<()> {
        let w = ward.to_vec();
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE guardianships SET ward_utc_offset_minutes = ?2
             WHERE supervised_actor_id = ?1",
            rusqlite::params![w, offset_minutes],
        )
        .context("set ward utc offset")?;
        Ok(())
    }

    /// Set or clear the guardian-enrolled-device marker on one of the ward's
    /// devices (`family-safety.md` § Full visibility). `Ok(false)` means no row
    /// matched — the device is not this ward's, or the account is not
    /// supervised — which the handler surfaces as `not_found`.
    ///
    /// Like every family write, a **no-op for an unsupervised account** (the
    /// `EXISTS` guardianship guard): a mark means nothing without a link to
    /// enforce it, so no adult's device row ever accrues one. This is why the
    /// removal-refusal predicate is `marked AND currently supervised` rather
    /// than the flag alone — see [`Self::graduate`].
    pub async fn set_device_guardian_mark(
        &self,
        ward: &[u8],
        device_id: &[u8],
        marked: bool,
    ) -> Result<bool> {
        let w = ward.to_vec();
        let d = device_id.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE sync_devices SET guardian_marked = ?3
                 WHERE actor_id = ?1 AND device_id = ?2
                   AND EXISTS (SELECT 1 FROM guardianships WHERE supervised_actor_id = ?1)",
                rusqlite::params![w, d, i64::from(marked)],
            )
            .context("set device guardian mark")?;
        Ok(n > 0)
    }

    /// Every currently-marked device of `ward` (`family-safety.md` § Full
    /// visibility) — the graduation handler's revoke list.
    ///
    /// Deliberately **unguarded** by the guardianship link: it is a read, and
    /// its one caller runs it while the link still exists (revoke-before-drop).
    /// Reading raw rows is also what lets [`Self::graduate`]'s tripwire count
    /// what the handler left behind.
    pub async fn list_marked_devices(&self, ward: &[u8]) -> Result<Vec<Vec<u8>>> {
        let w = ward.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT device_id FROM sync_devices
                 WHERE actor_id = ?1 AND guardian_marked != 0 ORDER BY registered_at",
            )
            .context("prepare list_marked_devices")?;
        let rows = stmt
            .query_map(rusqlite::params![w], |row| row.get::<_, Vec<u8>>(0))
            .context("query list_marked_devices")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read marked device row")?);
        }
        Ok(out)
    }

    /// Is `address` a known mail sender for this ward?
    /// (`family-safety.md` § The mail gate — known = `guardian_mail_allowlist`,
    /// which the ward's own outbound mail auto-seeds.) The empty address — the
    /// SMTP null reverse-path `<>` — is **never** known: anyone on the internet
    /// can claim it, and reading it as known was a full gate bypass. The
    /// legitimate null-path case (a remote MTA bouncing mail the ward sent) is
    /// the DSN correlation in `guardian_mail_verdict`, not this predicate; and
    /// `add_mail_allowlist_entry` skips empty addresses, so no stored row can
    /// ever make `""` known either.
    pub async fn is_known_mail_sender(&self, ward: &[u8], address: &str) -> Result<bool> {
        if address.is_empty() {
            return Ok(false);
        }
        let w = ward.to_vec();
        let addr = normalize_mail_address(address);
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM guardian_mail_allowlist
                 WHERE supervised_actor_id = ?1 AND address = ?2",
                rusqlite::params![w, addr],
                |row| row.get(0),
            )
            .context("check known mail sender")?;
        Ok(n > 0)
    }

    /// Add `address` to a ward's known-sender set. Idempotent — a repeat send to
    /// the same correspondent keeps the original `created_at` and `added_by`.
    /// `added_by` is `"outbound"` (the ward mailed them) or `"guardian"` (an
    /// approve / pre-approval). A no-op for an unsupervised account, so callers
    /// need not pre-check the link.
    ///
    /// **The `"outbound"` auto-seed declines a correlated-delivery origin**.
    /// The auto-seed's premise — *"the ward chose to mail them, so replies
    /// flow"* — is false when the ward is replying to a null-path report that
    /// reached the INBOX through the sent-Message-ID correlation: there the
    /// report's *author* chose the addresses (`From:`/`Reply-To:`/`Cc:`), and
    /// seeding them would convert one budget-bounded delivery into permanent,
    /// guardian-invisible access. So an address recorded by
    /// [`Self::add_correlated_delivery_origins`] within its window is skipped
    /// — the ward's reply still *sends*, it just doesn't bootstrap the
    /// allowlist; nothing legitimate is lost, since nobody replies to a real
    /// `MAILER-DAEMON` bounce. The **guardian's own decisions are exempt**
    /// (`added_by = "guardian"`): an explicit approve/pre-approve is consent,
    /// exactly what the suppression preserves the need for.
    pub async fn add_mail_allowlist_entry(
        &self,
        ward: &[u8],
        address: &str,
        added_by: &str,
    ) -> Result<()> {
        if address.is_empty() {
            return Ok(());
        }
        let w = ward.to_vec();
        let addr = normalize_mail_address(address);
        let by = added_by.to_string();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO guardian_mail_allowlist
                 (supervised_actor_id, address, added_by, created_at)
             SELECT ?1, ?2, ?3, ?4
             WHERE EXISTS (SELECT 1 FROM guardianships WHERE supervised_actor_id = ?1)
               AND (?3 != 'outbound' OR NOT EXISTS (
                     SELECT 1 FROM guardian_mail_correlated_origins
                     WHERE supervised_actor_id = ?1 AND address = ?2
                       AND created_at >= ?5))",
            rusqlite::params![w, addr, by, now, now - SENT_MSGID_RETENTION_SECS],
        )
        .context("add mail allowlist entry")?;
        Ok(())
    }

    /// Record the address-header set of a **delivered** correlated null-path
    /// report (`family-safety.md` § The mail gate). These are the only
    /// addresses a one-click reply (or reply-all) to that report can be sent
    /// to, and [`Self::add_mail_allowlist_entry`]'s `"outbound"` seed declines
    /// them.
    ///
    /// A repeat recording refreshes `created_at` (the reply threat is relative
    /// to the *latest* delivery, and each delivery already cost the report a
    /// correlation-budget unit, so refreshing extends nothing an attacker
    /// hasn't paid for). Rows are read through the same
    /// [`SENT_MSGID_RETENTION_SECS`] window as every other correlation
    /// artifact, pruned opportunistically on the way in, and — like every
    /// table here — a no-op for an unsupervised account.
    ///
    /// The poisoning angle is deliberate and bounded: an attacker who lists a
    /// ward's *future* correspondent in the report headers merely delays that
    /// correspondent's allowlist seed — their replies then hold for guardian
    /// release (fail-toward-holding), and the guardian's approve overrides
    /// the suppression.
    pub async fn add_correlated_delivery_origins(
        &self,
        ward: &[u8],
        addresses: &[String],
    ) -> Result<()> {
        let w = ward.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        for address in addresses {
            let addr = normalize_mail_address(address);
            if addr.is_empty() {
                continue;
            }
            conn.execute(
                "INSERT INTO guardian_mail_correlated_origins
                     (supervised_actor_id, address, created_at)
                 SELECT ?1, ?2, ?3
                 WHERE EXISTS (SELECT 1 FROM guardianships WHERE supervised_actor_id = ?1)
                 ON CONFLICT (supervised_actor_id, address)
                 DO UPDATE SET created_at = excluded.created_at",
                rusqlite::params![w, addr, now],
            )
            .context("add correlated delivery origin")?;
        }
        conn.execute(
            "DELETE FROM guardian_mail_correlated_origins
             WHERE supervised_actor_id = ?1 AND created_at < ?2",
            rusqlite::params![w, now - SENT_MSGID_RETENTION_SECS],
        )
        .context("prune correlated delivery origins")?;
        Ok(())
    }

    /// Record that `ward` sent a message with RFC 5322 Message-ID `message_id`
    /// to `remote_recipients` addresses outside this nest (`family-safety.md`
    /// § The mail gate — the null-path correlation).
    ///
    /// This is the *unforgeable* half of the mail gate's known-correspondent
    /// notion. Its sibling [`Self::add_mail_allowlist_entry`] records the
    /// *address* the ward mailed, which lets replies through; but an address is
    /// public knowledge, so a stranger can claim to bounce mail addressed to it.
    /// A Message-ID a Fauna app minted is a 128-bit random token, so naming
    /// it proves proximity to the message itself.
    ///
    /// Three gates on the way in, each fail-closed toward *holding* a later
    /// report rather than delivering it:
    ///
    /// - **Nothing to bounce, nothing to seed.** `remote_recipients == 0` (an
    ///   in-domain-only message never enters the outbound queue, so no remote
    ///   MTA can legitimately bounce it) records nothing.
    /// - **Only ids a Fauna path verifiably minted.** An id whose embedded
    ///   tag fails [`fauna_mail::msgid::is_fauna_minted_msgid`] — a
    ///   *verification*, not a shape match; an MD5/`uuid4().hex` lookalike
    ///   fails it — came from a third-party MUA whose entropy is unknowable:
    ///   undecidable as "weak?", decidable as "ours?". It is never seeded;
    ///   such a ward's real bounces are held for guardian release.
    /// - **A budget, not a durable fact.** RFC 5322 threading leaks the id in
    ///   `References:` to every later thread participant — exactly the
    ///   adjacent-but-unchosen population the gate exists to exclude — so each
    ///   seed carries `remote_recipients + 2` correlated deliveries (room for
    ///   every real per-recipient bounce plus a delayed/failed pair), consumed
    ///   by [`Self::consume_sent_msgid_correlation`].
    ///
    /// Seeded at the same two outbound chokepoints as the allowlist, and like it
    /// a **no-op for an unsupervised account** (the `WHERE EXISTS` guard), so no
    /// adult's mail metadata is retained and callers need not pre-check the link.
    /// Idempotent (a re-seed never stacks budget); an absent or blank Message-ID
    /// records nothing.
    ///
    /// Prunes this ward's rows past [`SENT_MSGID_RETENTION_SECS`] on the way in
    /// — the *storage* bound (no background task). The *security* window is
    /// enforced on the read path, where it holds even for a ward who stopped
    /// sending.
    pub async fn add_sent_msgid(
        &self,
        ward: &[u8],
        message_id: &str,
        remote_recipients: usize,
    ) -> Result<()> {
        if remote_recipients == 0 {
            return Ok(());
        }
        let Some(mid) = fauna_mail::dedup_key::normalize_message_id(message_id) else {
            return Ok(());
        };
        if !fauna_mail::msgid::is_fauna_minted_msgid(&mid) {
            return Ok(());
        }
        let w = ward.to_vec();
        let now = now_epoch_secs();
        let budget = remote_recipients as i64 + 2;
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO guardian_mail_sent_msgids
                 (supervised_actor_id, message_id, created_at, correlation_budget)
             SELECT ?1, ?2, ?3, ?4
             WHERE EXISTS (SELECT 1 FROM guardianships WHERE supervised_actor_id = ?1)",
            rusqlite::params![w, mid, now, budget],
        )
        .context("add sent msgid")?;
        conn.execute(
            "DELETE FROM guardian_mail_sent_msgids
             WHERE supervised_actor_id = ?1 AND created_at < ?2",
            rusqlite::params![w, now - SENT_MSGID_RETENTION_SECS],
        )
        .context("prune sent msgids")?;
        Ok(())
    }

    /// Correlate — and **consume** — a null-reverse-path report against the
    /// ward's sent-Message-ID set: the authorizing fact for delivering it
    /// (`family-safety.md` § The mail gate). `true` exactly when `ward` sent a
    /// message with this id **within the retention window** and its
    /// correlation budget was still open; the successful probe spends one unit.
    ///
    /// One atomic `UPDATE`, so two concurrent reports can never both ride the
    /// last unit. The [`SENT_MSGID_RETENTION_SECS`] window is a predicate
    /// *here*, not only in the seed-side prune: the prune fires when the ward
    /// next sends, so without the read-side bound a ward who stopped sending
    /// would keep every old id correlatable indefinitely.
    ///
    /// Fails **closed** on a blank or unnormalizable id — an absent correlation
    /// is never a correlation. A spent or expired row stays until the prune;
    /// it correlates nothing.
    pub async fn consume_sent_msgid_correlation(
        &self,
        ward: &[u8],
        message_id: &str,
    ) -> Result<bool> {
        let Some(mid) = fauna_mail::dedup_key::normalize_message_id(message_id) else {
            return Ok(false);
        };
        let w = ward.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE guardian_mail_sent_msgids
                 SET correlation_budget = correlation_budget - 1
                 WHERE supervised_actor_id = ?1 AND message_id = ?2
                   AND correlation_budget > 0
                   AND created_at >= ?3",
                rusqlite::params![w, mid, now - SENT_MSGID_RETENTION_SECS],
            )
            .context("consume sent-msgid correlation")?;
        Ok(n > 0)
    }

    /// Every message currently held for `ward`, oldest first — the guardian's
    /// `mail_hold` queue (`family-safety.md` § Reach approvals). Envelope
    /// metadata only; the message body is sealed to the ward and the nest never
    /// holds a key for it.
    pub async fn list_mail_holds(&self, ward: &[u8]) -> Result<Vec<MailHoldRow>> {
        let ward = ward.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT message_id, supervised_actor_id, sender_address, created_at
                 FROM guardian_mail_holds WHERE supervised_actor_id = ?1
                 ORDER BY created_at ASC, message_id ASC",
            )
            .context("prepare list mail holds")?;
        let rows = stmt
            .query_map(rusqlite::params![ward], |row| {
                Ok(MailHoldRow {
                    message_id: row.get(0)?,
                    supervised_actor_id: row.get(1)?,
                    sender_address: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })
            .context("query mail holds")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("read mail hold row")?);
        }
        Ok(out)
    }

    /// One hold, looked up by the `(ward, message_id)` pair the guardian's
    /// decide call names. Ward-scoped on purpose: a guardian of one ward must
    /// never be able to decide another ward's held message by guessing its id.
    pub async fn get_mail_hold(
        &self,
        ward: &[u8],
        message_id: &[u8; 32],
    ) -> Result<Option<MailHoldRow>> {
        let ward = ward.to_vec();
        let message_id = message_id.to_vec();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT message_id, supervised_actor_id, sender_address, created_at
             FROM guardian_mail_holds
             WHERE supervised_actor_id = ?1 AND message_id = ?2",
            rusqlite::params![ward, message_id],
            |row| {
                Ok(MailHoldRow {
                    message_id: row.get(0)?,
                    supervised_actor_id: row.get(1)?,
                    sender_address: row.get(2)?,
                    created_at: row.get(3)?,
                })
            },
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get mail hold"),
        }
    }

    /// Retire one hold's sidecar row once its message has been released to INBOX
    /// or discarded. Idempotent — returns false when the row was already gone.
    pub async fn delete_mail_hold(&self, ward: &[u8], message_id: &[u8; 32]) -> Result<bool> {
        let ward = ward.to_vec();
        let message_id = message_id.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM guardian_mail_holds
                 WHERE supervised_actor_id = ?1 AND message_id = ?2",
                rusqlite::params![ward, message_id],
            )
            .context("delete mail hold")?;
        Ok(n > 0)
    }

    /// Graduation (`family-safety.md` § Graduation & transfer): drop the
    /// link, the policy row and the known-sender set in ONE transaction,
    /// touching nothing else on the account. Returns false when the account was
    /// not supervised.
    ///
    /// **Callers must release every held message first** (the handler moves each
    /// one to INBOX via `release_mail_hold`, then calls this). The `ensure!`
    /// below is the fail-closed tripwire that keeps *"graduation releases, never
    /// drops"* (`family-safety.md` § Reach approvals) true against a future
    /// caller that forgets: dropping the sidecar rows here while their messages
    /// sat in the held mailbox would strand mail the ward can no longer surface
    /// through any queue. Releasing before graduating is also the crash-safe
    /// order — a crash between the two leaves the mail in INBOX and the link
    /// intact, and re-running graduation is a no-op replay.
    ///
    /// **Callers must likewise revoke every marked device first** (the handler
    /// deletes each one, then calls this) — the second `ensure!` is the same
    /// tripwire for the same reason (`family-safety.md` § Full visibility rule
    /// b). It cannot be done *here*: revocation must also drop the device's live
    /// connection, and a DB transaction has no reach into one. The ordering is
    /// crash-safe in the same shape — a crash between revoke and drop leaves the
    /// device revoked and the link intact, and the replay is a no-op.
    pub async fn graduate(&self, supervised: &[u8]) -> Result<bool> {
        let supervised = supervised.to_vec();
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction().context("begin graduate tx")?;

        let held: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM guardian_mail_holds WHERE supervised_actor_id = ?1",
                rusqlite::params![supervised],
                |row| row.get(0),
            )
            .context("count mail holds at graduation")?;
        anyhow::ensure!(
            held == 0,
            "graduation would drop {held} held message(s) — release them to INBOX first \
             (family-safety.md § Reach approvals)"
        );

        let marked: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sync_devices
                 WHERE actor_id = ?1 AND guardian_marked != 0",
                rusqlite::params![supervised],
                |row| row.get(0),
            )
            .context("count marked devices at graduation")?;
        anyhow::ensure!(
            marked == 0,
            "graduation would strand {marked} marked guardian device(s) — revoke them \
             first (family-safety.md § Full visibility)"
        );

        let n = tx
            .execute(
                "DELETE FROM guardianships WHERE supervised_actor_id = ?1",
                rusqlite::params![supervised],
            )
            .context("delete guardianship")?;
        tx.execute(
            "DELETE FROM guardian_policies WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete guardian policy")?;
        tx.execute(
            "DELETE FROM guardian_mail_allowlist WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete guardian mail allowlist")?;
        tx.execute(
            "DELETE FROM guardian_mail_sent_msgids WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete guardian mail sent msgids")?;
        tx.execute(
            "DELETE FROM guardian_mail_correlated_origins WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete guardian mail correlated origins")?;
        // Guardian Notify counts die with the link — the graduated account is no
        // longer supervised, so its enforcement telemetry has no reader
        // (family-safety.md § Guardian Notify — dropped at graduation).
        tx.execute(
            "DELETE FROM guardian_content_notices WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete guardian content notices at graduation")?;
        // Screen-time usage accounting dies with the link for the same reason
        // (family-safety.md § Screen time — dropped at graduation).
        tx.execute(
            "DELETE FROM guardian_usage WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete guardian usage at graduation")?;
        // A graduated account has no guardianship to transfer — a pending
        // proposal for it dies with the link.
        tx.execute(
            "DELETE FROM guardian_transfers WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete pending transfer at graduation")?;
        // A graduated account contacts whoever it likes — a pending contact
        // ask is oversight intent the link's end makes moot (dropped, not
        // released — family-safety.md § Child-initiated contact requests).
        tx.execute(
            "DELETE FROM guardian_contact_requests WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete pending contact requests at graduation")?;
        // Likewise a graduated account adds whatever sources it likes: both the
        // asks and any unredeemed grants are moot (family-safety.md
        // § Feed-source approvals). Dropping the *grants* loses the ward
        // nothing — the gate they unlock no longer fires at all.
        tx.execute(
            "DELETE FROM guardian_feed_requests WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete feed requests at graduation")?;
        // A graduated account DMs whoever it likes: the per-peer verdicts are
        // oversight decisions the link's end makes moot (family-safety.md § The
        // bridge-DM gate). Dropped, not released — and unlike a mail hold there
        // is nothing to release: no DM was ever withheld, only *marked*, so the
        // drop destroys no user data. It cannot strand the account either — the
        // gate stops firing the moment the link is gone (`supervised_dm_verdict`
        // short-circuits on an unsupervised policy), so even a stale row left by
        // a future caller would be inert.
        tx.execute(
            "DELETE FROM guardian_dm_peers WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete dm peer verdicts at graduation")?;
        // The age band dies with the link (family-safety.md § The account age
        // band): a graduated account is `18+`/`none` **by construction** (the
        // no-link rule), and a surviving minor-band row would contradict it.
        // The band was only ever a defaults dial at admission — nothing
        // enforces from it, so the drop lifts no live rule.
        tx.execute(
            "DELETE FROM account_age_bands WHERE actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("delete age band at graduation")?;
        tx.commit().context("commit graduation")?;
        Ok(n > 0)
    }

    /// Transfer (`family-safety.md` § Graduation & transfer): re-point the
    /// ward's link to a new (already-validated) guardian, keeping the policy
    /// document intact. Returns false when the account was not supervised.
    pub async fn transfer_guardian(&self, supervised: &[u8], new_guardian: &[u8]) -> Result<bool> {
        let supervised = supervised.to_vec();
        let new_guardian = new_guardian.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "UPDATE guardianships SET guardian_actor_id = ?2
                 WHERE supervised_actor_id = ?1",
                rusqlite::params![supervised, new_guardian],
            )
            .context("transfer guardianship")?;
        Ok(n > 0)
    }

    /// Record (or replace) the ward's pending transfer proposal
    /// (`family-safety.md` § Graduation & transfer). The PK upsert IS the
    /// one-pending-per-ward invariant — a new proposal supersedes the old one,
    /// whose target can no longer accept it. Prunes expired rows table-wide on
    /// the way in (the same lazy-expiry shape as [`Self::add_sent_msgid`]).
    pub async fn upsert_pending_transfer(
        &self,
        supervised: &[u8],
        proposed_guardian: &[u8],
        initiated_by: &[u8],
    ) -> Result<()> {
        let supervised = supervised.to_vec();
        let proposed = proposed_guardian.to_vec();
        let initiator = initiated_by.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM guardian_transfers WHERE created_at < ?1",
            rusqlite::params![now - PENDING_TRANSFER_TTL_SECS],
        )
        .context("prune expired pending transfers")?;
        conn.execute(
            "INSERT OR REPLACE INTO guardian_transfers
                 (supervised_actor_id, proposed_guardian_actor_id, initiated_by, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![supervised, proposed, initiator, now],
        )
        .context("upsert pending transfer")?;
        Ok(())
    }

    /// The ward's pending transfer proposal, if one is live (expired rows are
    /// invisible — lazy expiry).
    pub async fn get_pending_transfer(
        &self,
        supervised: &[u8],
    ) -> Result<Option<GuardianTransferRow>> {
        let supervised = supervised.to_vec();
        let cutoff = now_epoch_secs() - PENDING_TRANSFER_TTL_SECS;
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT supervised_actor_id, proposed_guardian_actor_id, initiated_by, created_at
             FROM guardian_transfers
             WHERE supervised_actor_id = ?1 AND created_at >= ?2",
            rusqlite::params![supervised, cutoff],
            |row| {
                Ok(GuardianTransferRow {
                    supervised_actor_id: row.get(0)?,
                    proposed_guardian_actor_id: row.get(1)?,
                    initiated_by: row.get(2)?,
                    created_at: row.get(3)?,
                })
            },
        );
        match result {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get pending transfer"),
        }
    }

    /// Every live proposal awaiting `proposed_guardian`'s consent, oldest
    /// first — the incoming-transfer prompt's read.
    pub async fn list_incoming_transfers(
        &self,
        proposed_guardian: &[u8],
    ) -> Result<Vec<GuardianTransferRow>> {
        let proposed = proposed_guardian.to_vec();
        let cutoff = now_epoch_secs() - PENDING_TRANSFER_TTL_SECS;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT supervised_actor_id, proposed_guardian_actor_id, initiated_by, created_at
                 FROM guardian_transfers
                 WHERE proposed_guardian_actor_id = ?1 AND created_at >= ?2
                 ORDER BY created_at",
            )
            .context("prepare list incoming transfers")?;
        let rows = stmt
            .query_map(rusqlite::params![proposed, cutoff], |row| {
                Ok(GuardianTransferRow {
                    supervised_actor_id: row.get(0)?,
                    proposed_guardian_actor_id: row.get(1)?,
                    initiated_by: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })
            .context("query incoming transfers")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read incoming transfer row")?);
        }
        Ok(results)
    }

    /// The proposed guardian consents: re-point the link and clear the pending
    /// row in **one transaction** — the handshake's single atomic decision
    /// point (`family-safety.md` § Graduation & transfer). Returns false when
    /// no live proposal names `caller` for this ward (none pending, expired,
    /// superseded, or the caller is not the proposed guardian), or when the
    /// ward is no longer supervised — in every false case nothing changed.
    pub async fn accept_pending_transfer(&self, supervised: &[u8], caller: &[u8]) -> Result<bool> {
        let supervised = supervised.to_vec();
        let caller = caller.to_vec();
        let cutoff = now_epoch_secs() - PENDING_TRANSFER_TTL_SECS;
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin accept transfer tx")?;
        let matched: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM guardian_transfers
                 WHERE supervised_actor_id = ?1
                   AND proposed_guardian_actor_id = ?2
                   AND created_at >= ?3",
                rusqlite::params![supervised, caller, cutoff],
                |row| row.get(0),
            )
            .context("match pending transfer")?;
        if matched == 0 {
            return Ok(false);
        }
        let repointed = tx
            .execute(
                "UPDATE guardianships SET guardian_actor_id = ?2
                 WHERE supervised_actor_id = ?1",
                rusqlite::params![supervised, caller],
            )
            .context("re-point guardianship")?;
        if repointed == 0 {
            // A live proposal for an unsupervised ward should be unreachable
            // (graduation/deletion cascade the row) — fail safe, change nothing.
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM guardian_transfers WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("clear accepted transfer")?;
        tx.commit().context("commit accept transfer")?;
        Ok(true)
    }

    /// Drop the ward's pending proposal iff it names `caller` as the proposed
    /// guardian (the decline path). Returns false when no live proposal names
    /// them.
    pub async fn decline_pending_transfer(&self, supervised: &[u8], caller: &[u8]) -> Result<bool> {
        let supervised = supervised.to_vec();
        let caller = caller.to_vec();
        let cutoff = now_epoch_secs() - PENDING_TRANSFER_TTL_SECS;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM guardian_transfers
                 WHERE supervised_actor_id = ?1
                   AND proposed_guardian_actor_id = ?2
                   AND created_at >= ?3",
                rusqlite::params![supervised, caller, cutoff],
            )
            .context("decline pending transfer")?;
        Ok(n > 0)
    }

    /// Withdraw the ward's pending proposal (the cancel path — caller
    /// authorization is the handler's guardian-or-admin gate). Returns false
    /// when none was live.
    pub async fn cancel_pending_transfer(&self, supervised: &[u8]) -> Result<bool> {
        let supervised = supervised.to_vec();
        let cutoff = now_epoch_secs() - PENDING_TRANSFER_TTL_SECS;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM guardian_transfers
                 WHERE supervised_actor_id = ?1 AND created_at >= ?2",
                rusqlite::params![supervised, cutoff],
            )
            .context("cancel pending transfer")?;
        Ok(n > 0)
    }

    /// Record the ward's contact ask (`family-safety.md` § Child-initiated
    /// contact requests). Guardianship-guarded (a no-op for an unsupervised
    /// caller), deduped by the PK (a re-ask while pending is
    /// [`ContactRequestAdd::AlreadyPending`] — no re-ring), capped per ward,
    /// and pruning expired rows table-wide on the way in (the
    /// [`Self::upsert_pending_transfer`] lazy-expiry shape). All the checks
    /// run under the one connection lock, so the outcome is race-free.
    pub async fn add_contact_request(
        &self,
        supervised: &[u8],
        peer: &[u8],
    ) -> Result<ContactRequestAdd> {
        let supervised = supervised.to_vec();
        let peer = peer.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM guardian_contact_requests WHERE created_at < ?1",
            rusqlite::params![now - PENDING_CONTACT_REQUEST_TTL_SECS],
        )
        .context("prune expired contact requests")?;
        let supervised_now: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM guardianships WHERE supervised_actor_id = ?1",
                rusqlite::params![supervised],
                |row| row.get(0),
            )
            .context("contact request guardianship guard")?;
        if supervised_now == 0 {
            return Ok(ContactRequestAdd::NotSupervised);
        }
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM guardian_contact_requests
                 WHERE supervised_actor_id = ?1",
                rusqlite::params![supervised],
                |row| row.get(0),
            )
            .context("count pending contact requests")?;
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO guardian_contact_requests
                     (supervised_actor_id, peer_actor_id, created_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![supervised, peer, now],
            )
            .context("insert contact request")?;
        if n == 0 {
            return Ok(ContactRequestAdd::AlreadyPending);
        }
        if pending >= MAX_PENDING_CONTACT_REQUESTS {
            // The row above only landed because the cap check said full AFTER
            // the insert would exceed it — undo and refuse. (Check-then-insert
            // under the same lock; the undo keeps the two statements simple.)
            conn.execute(
                "DELETE FROM guardian_contact_requests
                 WHERE supervised_actor_id = ?1 AND peer_actor_id = ?2",
                rusqlite::params![supervised, peer],
            )
            .context("undo over-cap contact request")?;
            return Ok(ContactRequestAdd::CapExceeded);
        }
        Ok(ContactRequestAdd::Created {
            row_id: conn.last_insert_rowid(),
        })
    }

    /// The ward's live pending asks, oldest first (expired rows invisible —
    /// lazy expiry).
    pub async fn list_contact_requests(&self, supervised: &[u8]) -> Result<Vec<ContactRequestRow>> {
        let supervised = supervised.to_vec();
        let cutoff = now_epoch_secs() - PENDING_CONTACT_REQUEST_TTL_SECS;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT supervised_actor_id, peer_actor_id, created_at
                 FROM guardian_contact_requests
                 WHERE supervised_actor_id = ?1 AND created_at >= ?2
                 ORDER BY created_at",
            )
            .context("prepare list contact requests")?;
        let rows = stmt
            .query_map(rusqlite::params![supervised, cutoff], |row| {
                Ok(ContactRequestRow {
                    supervised_actor_id: row.get(0)?,
                    peer_actor_id: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })
            .context("query contact requests")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read contact request row")?);
        }
        Ok(results)
    }

    /// Drop one pending ask (the decide path — approve and deny both remove
    /// the row). Returns false when no live ask matches (none, expired, or a
    /// guessed peer) — the handler's `not_found`.
    pub async fn delete_contact_request(&self, supervised: &[u8], peer: &[u8]) -> Result<bool> {
        let supervised = supervised.to_vec();
        let peer = peer.to_vec();
        let cutoff = now_epoch_secs() - PENDING_CONTACT_REQUEST_TTL_SECS;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM guardian_contact_requests
                 WHERE supervised_actor_id = ?1 AND peer_actor_id = ?2
                   AND created_at >= ?3",
                rusqlite::params![supervised, peer, cutoff],
            )
            .context("delete contact request")?;
        Ok(n > 0)
    }

    // ── Feed-source approvals (family-safety.md § Feed-source approvals) ──
    //
    // Every query below states its own expiry window in its `WHERE` clause
    // rather than trusting the opportunistic prune in `add_feed_request` to have
    // run. That is deliberate and load-bearing: a prune that only fires on the
    // next *write* leaves rows live indefinitely for a ward who stopped writing
    // — a known gap, whose fix moved the window onto the read probe itself. The
    // windows are pinned per path by `feed_requests_windows_are_enforced_on_
    // every_read_path`.

    /// Record a ward's feed-source ask (`family-safety.md` § Feed-source
    /// approvals). Guardianship-guarded (a no-op for an unsupervised caller),
    /// deduped by the UNIQUE key (a re-ask while an ask is open is
    /// [`FeedRequestAdd::AlreadyOpen`] — no re-ring), capped per ward, and
    /// pruning expired rows table-wide on the way in — the
    /// [`Self::add_contact_request`] shape. All checks run under the one
    /// connection lock, so the outcome is race-free.
    ///
    /// The prune is what makes a re-ask after a lapse a *fresh* row (new
    /// AUTOINCREMENT id → the doorbell rings again), rather than an
    /// `AlreadyOpen` no-op against a corpse.
    pub async fn add_feed_request(
        &self,
        supervised: &[u8],
        bridge_id: &str,
        operation: &str,
        target: &str,
        label: &str,
    ) -> Result<FeedRequestAdd> {
        let supervised = supervised.to_vec();
        let (bridge_id, operation, target, label) = (
            bridge_id.to_string(),
            operation.to_string(),
            target.to_string(),
            label.to_string(),
        );
        let now = now_epoch_secs();
        let pending_cutoff = now - PENDING_FEED_REQUEST_TTL_SECS;
        let grant_cutoff = now - FEED_GRANT_TTL_SECS;
        let conn = self.conn.lock().await;
        // Prune both kinds of dead row: a lapsed pending ask, and a grant whose
        // 7 days ran out unredeemed.
        conn.execute(
            "DELETE FROM guardian_feed_requests
             WHERE (approved_at IS     NULL AND created_at  < ?1)
                OR (approved_at IS NOT NULL AND approved_at < ?2)",
            rusqlite::params![pending_cutoff, grant_cutoff],
        )
        .context("prune expired feed requests")?;
        let supervised_now: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM guardianships WHERE supervised_actor_id = ?1",
                rusqlite::params![supervised],
                |row| row.get(0),
            )
            .context("feed request guardianship guard")?;
        if supervised_now == 0 {
            return Ok(FeedRequestAdd::NotSupervised);
        }
        let open: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM guardian_feed_requests
                 WHERE supervised_actor_id = ?1
                   AND ((approved_at IS     NULL AND created_at  >= ?2)
                     OR (approved_at IS NOT NULL AND approved_at >= ?3))",
                rusqlite::params![supervised, pending_cutoff, grant_cutoff],
                |row| row.get(0),
            )
            .context("count open feed requests")?;
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO guardian_feed_requests
                     (supervised_actor_id, bridge_id, operation, target, label,
                      created_at, approved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                rusqlite::params![supervised, bridge_id, operation, target, label, now],
            )
            .context("insert feed request")?;
        if n == 0 {
            return Ok(FeedRequestAdd::AlreadyOpen);
        }
        if open >= MAX_PENDING_FEED_REQUESTS {
            // Check-then-insert-then-undo under the same lock, mirroring
            // `add_contact_request`: the undo keeps both statements simple.
            conn.execute(
                "DELETE FROM guardian_feed_requests
                 WHERE supervised_actor_id = ?1 AND bridge_id = ?2
                   AND operation = ?3 AND target = ?4",
                rusqlite::params![supervised, bridge_id, operation, target],
            )
            .context("undo over-cap feed request")?;
            return Ok(FeedRequestAdd::CapExceeded);
        }
        Ok(FeedRequestAdd::Created {
            row_id: conn.last_insert_rowid(),
        })
    }

    /// The ward's live feed-source rows — pending asks *and* unredeemed grants
    /// — oldest first. Dead rows are invisible (lazy expiry, enforced here).
    ///
    /// `pending_only` narrows to rows still awaiting the guardian: that is the
    /// guardian's queue, since a granted row is waiting on the *ward's* retry,
    /// not on any decision. The ward's own status read passes `false` and sees
    /// both states.
    pub async fn list_feed_requests(
        &self,
        supervised: &[u8],
        pending_only: bool,
    ) -> Result<Vec<FeedRequestRow>> {
        let supervised = supervised.to_vec();
        let now = now_epoch_secs();
        let pending_cutoff = now - PENDING_FEED_REQUEST_TTL_SECS;
        let grant_cutoff = now - FEED_GRANT_TTL_SECS;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                // ?4 = pending_only: when set, granted rows drop out entirely.
                "SELECT supervised_actor_id, bridge_id, operation, target, label,
                        created_at, approved_at
                 FROM guardian_feed_requests
                 WHERE supervised_actor_id = ?1
                   AND (   (approved_at IS     NULL AND created_at  >= ?2)
                        OR (approved_at IS NOT NULL AND approved_at >= ?3
                            AND ?4 = 0))
                 ORDER BY created_at",
            )
            .context("prepare list feed requests")?;
        let rows = stmt
            .query_map(
                rusqlite::params![supervised, pending_cutoff, grant_cutoff, pending_only],
                |row| {
                    Ok(FeedRequestRow {
                        supervised_actor_id: row.get(0)?,
                        bridge_id: row.get(1)?,
                        operation: row.get(2)?,
                        target: row.get(3)?,
                        label: row.get(4)?,
                        created_at: row.get(5)?,
                        approved_at: row.get(6)?,
                    })
                },
            )
            .context("query feed requests")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read feed request row")?);
        }
        Ok(results)
    }

    /// Approve one live **pending** ask, turning it into a grant dated now
    /// (`family-safety.md` § Feed-source approvals — approve mints a grant, it
    /// never performs the operation). Returns the approved row's AUTOINCREMENT
    /// id, or `None` when no live pending ask matches (none, expired, already
    /// granted, or a guessed key) — the handler's `not_found`.
    ///
    /// The id is returned, rather than a bare bool, because the **ward's**
    /// doorbell dedups on it: a ward who asks for the same object again after
    /// redeeming an earlier grant must be rung again, and every key derivable
    /// from the ask itself (the object triple, the approval second) would
    /// collide with the previous approval and be silently swallowed. This is the
    /// v23 contact-ask lesson — an id is the only thing unique per *decision*.
    ///
    /// Deliberately pending-only: re-approving a live grant would silently
    /// restart its 7-day window, so a guardian double-click would extend
    /// standing permission rather than be the no-op it looks like.
    pub async fn approve_feed_request(
        &self,
        supervised: &[u8],
        bridge_id: &str,
        operation: &str,
        target: &str,
    ) -> Result<Option<i64>> {
        let supervised = supervised.to_vec();
        let (bridge_id, operation, target) = (
            bridge_id.to_string(),
            operation.to_string(),
            target.to_string(),
        );
        let now = now_epoch_secs();
        let pending_cutoff = now - PENDING_FEED_REQUEST_TTL_SECS;
        let conn = self.conn.lock().await;
        conn.query_row(
            "UPDATE guardian_feed_requests SET approved_at = ?1
             WHERE supervised_actor_id = ?2 AND bridge_id = ?3
               AND operation = ?4 AND target = ?5
               AND approved_at IS NULL AND created_at >= ?6
             RETURNING id",
            rusqlite::params![
                now,
                supervised,
                bridge_id,
                operation,
                target,
                pending_cutoff
            ],
            |row| row.get(0),
        )
        .optional()
        .context("approve feed request")
    }

    /// Drop one live **pending** ask (the decide-deny path). Returns false when
    /// no live pending ask matches — the handler's `not_found`.
    pub async fn delete_feed_request(
        &self,
        supervised: &[u8],
        bridge_id: &str,
        operation: &str,
        target: &str,
    ) -> Result<bool> {
        let supervised = supervised.to_vec();
        let (bridge_id, operation, target) = (
            bridge_id.to_string(),
            operation.to_string(),
            target.to_string(),
        );
        let pending_cutoff = now_epoch_secs() - PENDING_FEED_REQUEST_TTL_SECS;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM guardian_feed_requests
                 WHERE supervised_actor_id = ?1 AND bridge_id = ?2
                   AND operation = ?3 AND target = ?4
                   AND approved_at IS NULL AND created_at >= ?5",
                rusqlite::params![supervised, bridge_id, operation, target, pending_cutoff],
            )
            .context("delete feed request")?;
        Ok(n > 0)
    }

    /// **Spend** a live grant for exactly this object, if one exists: an atomic
    /// delete returning whether it fired (`family-safety.md` § Feed-source
    /// approvals — *"the gate looks for a matching unexpired grant and consumes
    /// it before the operation runs"*).
    ///
    /// Three properties this one statement carries, each load-bearing:
    ///
    /// - **Single-use.** The consume *is* the delete, so two concurrent retries
    ///   cannot both see the grant — SQLite serializes them and the second
    ///   `DELETE` matches nothing. A `SELECT`-then-`DELETE` would leave exactly
    ///   the double-spend window this shape closes.
    /// - **Consume-before-perform.** The caller spends the grant *before* the
    ///   provider call, so a crash between them burns it and the ward re-asks
    ///   (fail-closed). Perform-first would be the double-spend.
    /// - **Exact match.** The key is the whole `(bridge, operation, target)`
    ///   triple the guardian approved — never `label`, which is display-only —
    ///   so a grant for one follow can never unlock another.
    ///
    /// The 7-day window is enforced right here rather than left to the prune, so
    /// a lapsed grant is unspendable even on a nest where nothing has written
    /// since.
    pub async fn consume_feed_grant(
        &self,
        supervised: &[u8],
        bridge_id: &str,
        operation: &str,
        target: &str,
    ) -> Result<bool> {
        let supervised = supervised.to_vec();
        let (bridge_id, operation, target) = (
            bridge_id.to_string(),
            operation.to_string(),
            target.to_string(),
        );
        let grant_cutoff = now_epoch_secs() - FEED_GRANT_TTL_SECS;
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM guardian_feed_requests
                 WHERE supervised_actor_id = ?1 AND bridge_id = ?2
                   AND operation = ?3 AND target = ?4
                   AND approved_at IS NOT NULL AND approved_at >= ?5",
                rusqlite::params![supervised, bridge_id, operation, target, grant_cutoff],
            )
            .context("consume feed grant")?;
        Ok(n > 0)
    }

    // ── The bridge-DM gate's verdict set (v25) ─────────────────────────
    //
    // `guardian_dm_peers` stores *decisions*, never hold state: hold-ness is
    // computed from (knob, verdict row) at read time by
    // `fauna_core::data::supervised_dm_verdict` (`family-safety.md` § The
    // bridge-DM gate; § Don't do these — "don't store bridge-DM hold state").

    /// The ward's stored verdict for one external DM peer, or `None` when no row
    /// exists — a **cold peer**, which is the only case the `unknown_peer_dm`
    /// knob governs.
    ///
    /// Returns the raw string rather than a parsed enum: the fail-closed
    /// resolution of a verdict this binary cannot name lives in one place,
    /// `supervised_dm_verdict`, and handing it the stored bytes is what keeps
    /// each call site from inventing its own degrade.
    pub async fn dm_peer_verdict(
        &self,
        supervised: &[u8],
        bridge_id: &str,
        peer_id: &str,
    ) -> Result<Option<String>> {
        let supervised = supervised.to_vec();
        let (bridge_id, peer_id) = (bridge_id.to_string(), peer_id.to_string());
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT verdict FROM guardian_dm_peers
             WHERE supervised_actor_id = ?1 AND bridge_id = ?2 AND peer_id = ?3",
            rusqlite::params![supervised, bridge_id, peer_id],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get dm peer verdict"),
        }
    }

    /// Seed an `allow` verdict from the ward's **own outbound DM send**
    /// (`family-safety.md` § The bridge-DM gate — *"`allow` rows are seeded by
    /// the ward's own outbound DM sends … the child initiating chose the
    /// correspondent, so replies always flow"*). The DM twin of the mail
    /// allowlist's outbound auto-seed.
    ///
    /// Two properties, both load-bearing and both carried by the one statement:
    ///
    /// - **Never overwrites a `block`.** `INSERT OR IGNORE` leaves *any* existing
    ///   row untouched, so a ward cannot launder a guardian's block by DMing the
    ///   peer. (An `INSERT OR REPLACE` here would silently reset the guardian's
    ///   decision — the `INSERT OR REPLACE` unlisted-column trap, in verdict
    ///   form.) It also keeps `created_at` at the first send rather than
    ///   re-stamping it on every message.
    /// - **A no-op for an unsupervised account** (the `WHERE EXISTS` guardianship
    ///   guard), like every family side table — so no adult's DM correspondents
    ///   are ever recorded, and the caller needs no pre-check.
    pub async fn seed_dm_peer_allow(
        &self,
        supervised: &[u8],
        bridge_id: &str,
        peer_id: &str,
    ) -> Result<()> {
        let supervised = supervised.to_vec();
        let (bridge_id, peer_id) = (bridge_id.to_string(), peer_id.to_string());
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO guardian_dm_peers
                 (supervised_actor_id, bridge_id, peer_id, verdict, added_by, created_at)
             SELECT ?1, ?2, ?3, 'allow', 'ward', ?4
             WHERE EXISTS (SELECT 1 FROM guardianships WHERE supervised_actor_id = ?1)",
            rusqlite::params![supervised, bridge_id, peer_id, now],
        )
        .context("seed dm peer allow")?;
        Ok(())
    }

    /// Write the **guardian's** verdict for one peer (`approve` → `allow`,
    /// `deny` → `block`). Unlike the ward's seed this deliberately *does*
    /// overwrite: it is the guardian's decision, and it must be able to reverse
    /// an earlier one (including their own).
    ///
    /// Guardianship-guarded like every family side table, so it is a no-op on an
    /// unsupervised account; returns whether a row was written, which the handler
    /// maps to `not_found` for a ward it does not guard.
    pub async fn set_dm_peer_verdict(
        &self,
        supervised: &[u8],
        bridge_id: &str,
        peer_id: &str,
        verdict: DmPeerVerdict,
    ) -> Result<bool> {
        let supervised = supervised.to_vec();
        let (bridge_id, peer_id) = (bridge_id.to_string(), peer_id.to_string());
        let verdict = verdict.as_str();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "INSERT INTO guardian_dm_peers
                     (supervised_actor_id, bridge_id, peer_id, verdict, added_by, created_at)
                 SELECT ?1, ?2, ?3, ?4, 'guardian', ?5
                 WHERE EXISTS (SELECT 1 FROM guardianships WHERE supervised_actor_id = ?1)
                 ON CONFLICT (supervised_actor_id, bridge_id, peer_id)
                 DO UPDATE SET verdict = excluded.verdict, added_by = 'guardian'",
                rusqlite::params![supervised, bridge_id, peer_id, verdict, now],
            )
            .context("set dm peer verdict")?;
        Ok(n > 0)
    }

    /// Every stored verdict for the ward, as `(bridge_id, peer_id, verdict)`.
    ///
    /// The queue's held list is **computed** from this plus the ward's stored DM
    /// conversations (a conversation whose peer appears here is not held), rather
    /// than read from a hold table — which is the whole point of the design: there
    /// is no stored hold state to drain or strand.
    pub async fn list_dm_peer_verdicts(
        &self,
        supervised: &[u8],
    ) -> Result<Vec<(String, String, String)>> {
        let supervised = supervised.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT bridge_id, peer_id, verdict FROM guardian_dm_peers
                 WHERE supervised_actor_id = ?1
                 ORDER BY bridge_id, peer_id",
            )
            .context("prepare list dm peer verdicts")?;
        let rows = stmt
            .query_map(rusqlite::params![supervised], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .context("query dm peer verdicts")?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.context("read dm peer verdict row")?);
        }
        Ok(out)
    }

    /// User-deletion cascade (`family-safety.md` § Lifecycle gates): drop the
    /// deleted account's guardianship link + policy row — and any pending
    /// transfer naming it as ward *or* proposed guardian — in one transaction.
    /// A no-op for an account with no family rows. Part of the shared
    /// `finalize_user_deletion` cleanup.
    pub async fn delete_family_rows_for_supervised(&self, supervised: &[u8]) -> Result<()> {
        let supervised = supervised.to_vec();
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin family cascade tx")?;
        tx.execute(
            "DELETE FROM guardianships WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade guardianship")?;
        tx.execute(
            "DELETE FROM guardian_policies WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade guardian policy")?;
        // The account is going away with its messages, so holds are dropped
        // here rather than released — unlike graduation, where the account
        // survives and the mail must reach it.
        tx.execute(
            "DELETE FROM guardian_mail_allowlist WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade guardian mail allowlist")?;
        tx.execute(
            "DELETE FROM guardian_mail_sent_msgids WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade guardian mail sent msgids")?;
        tx.execute(
            "DELETE FROM guardian_mail_correlated_origins WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade guardian mail correlated origins")?;
        tx.execute(
            "DELETE FROM guardian_mail_holds WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade guardian mail holds")?;
        tx.execute(
            "DELETE FROM guardian_content_notices WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade guardian content notices")?;
        tx.execute(
            "DELETE FROM guardian_usage WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade guardian usage")?;
        // Pending transfers die with either party: with the ward (nothing left
        // to transfer) and with the proposed guardian (nobody left to consent —
        // a suspended-but-not-deleted target is instead caught by accept-time
        // re-validation). This cascade runs for every deleted user, so both
        // arms live in one statement.
        tx.execute(
            "DELETE FROM guardian_transfers
             WHERE supervised_actor_id = ?1 OR proposed_guardian_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade pending transfers")?;
        // Pending contact asks die with either party too: with the ward
        // (nobody asking) and with the named peer (nobody left to contact —
        // approving would mint an edge to a deleted account).
        tx.execute(
            "DELETE FROM guardian_contact_requests
             WHERE supervised_actor_id = ?1 OR peer_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade pending contact requests")?;
        // Feed-source asks and grants die with the ward — and *only* the ward.
        // Unlike a contact ask, the thing asked for is an external bridge object
        // (a follow id, a feed URI), not an actor on this nest, so there is no
        // second party whose deletion could moot the row.
        tx.execute(
            "DELETE FROM guardian_feed_requests WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade feed requests")?;
        // DM verdicts die with the ward, and — like the feed asks and unlike a
        // contact ask — only with the ward: the peer is an external bridge
        // identity, not an actor on this nest, so no second party's deletion can
        // moot the row.
        tx.execute(
            "DELETE FROM guardian_dm_peers WHERE supervised_actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade dm peer verdicts")?;
        // The age band cascades with the account (family-safety.md § The
        // account age band) — admission metadata with no account left to
        // describe. Runs for every deleted user; a no-op without a row.
        tx.execute(
            "DELETE FROM account_age_bands WHERE actor_id = ?1",
            rusqlite::params![supervised],
        )
        .context("cascade age band")?;
        tx.commit().context("commit family cascade")?;
        Ok(())
    }

    /// The account's established age band — `Some((band, provenance))` when a
    /// row exists, `None` otherwise (`family-safety.md` § The account age
    /// band). **Absence is meaningful, not missing data**: no row + no
    /// guardianship link is `18+`/`none` by construction, and no row + a link
    /// is a band-less supervised admission (band unknown) — the caller decides which by
    /// consulting the link, this read never does.
    pub async fn get_age_band(&self, actor: &[u8]) -> Result<Option<(String, String)>> {
        let actor = actor.to_vec();
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT band, provenance FROM account_age_bands WHERE actor_id = ?1",
            rusqlite::params![actor],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        match result {
            Ok(pair) => Ok(Some(pair)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("get age band"),
        }
    }
}

/// Write the account's age-band row inside an already-open admission
/// transaction (`family-safety.md` § The account age band — bands are
/// established **at admission**, atomically with the account itself).
/// `INSERT OR REPLACE`: the PK is the actor, and the only legal re-write path
/// is the admission transaction itself (an actor cannot be admitted twice —
/// `is_actor_registered` refuses first), so the REPLACE arm exists for
/// crash-replay idempotence, not for post-admission edits (there is no
/// band-edit kind; graduation deletes).
pub(super) fn set_age_band_tx(
    tx: &rusqlite::Transaction<'_>,
    actor: &[u8],
    band: &str,
    provenance: &str,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO account_age_bands (actor_id, band, provenance, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![actor, band, provenance, now_epoch_secs()],
    )
    .context("insert age band")?;
    Ok(())
}

/// Canonicalize an envelope address for allowlist equality. Mail domains are
/// case-insensitive (RFC 5321 § 2.4) and every real-world MTA also folds the
/// local part, so an exact-bytes match would let `Alice@x.test` bypass an
/// allowlist holding `alice@x.test`. Trimmed of the angle brackets an envelope
/// address may still carry.
pub(crate) fn normalize_mail_address(address: &str) -> String {
    address
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_ascii_lowercase()
}

/// Insert the guardianship link + the fresh policy row inside an already-open
/// admission transaction — the single atomic decision point
/// (`nest/common.md` § Client-state recoverability): after any crash the
/// account either exists supervised or does not exist.
///
/// `defaults` is the age band's **defaults dial** (`family-safety.md` § The
/// account age band D2: a banded admission starts from
/// `ReachPolicy::age_band_defaults(band)` instead of the all-unsupervised-
/// equivalent row; the guardian then edits per-knob exactly as today —
/// enforcement reads the policy row either way). `None` keeps today's bare
/// default row (a band-less admission).
pub(super) fn insert_guardianship_tx(
    tx: &rusqlite::Transaction<'_>,
    supervised: &[u8],
    guardian: &[u8],
    defaults: Option<&fauna_protocol::family::ReachPolicy>,
) -> Result<()> {
    let now = now_epoch_secs();
    tx.execute(
        "INSERT INTO guardianships (supervised_actor_id, guardian_actor_id, created_at)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![supervised, guardian, now],
    )
    .context("insert guardianship")?;
    match defaults {
        None => {
            tx.execute(
                "INSERT INTO guardian_policies (supervised_actor_id, updated_at) VALUES (?1, ?2)",
                rusqlite::params![supervised, now],
            )
            .context("insert default guardian policy")?;
        }
        Some(p) => {
            // Same encoding as the policy.update path
            // (`family_handlers::validate_feature_sub_document`) — the stored
            // document must be indistinguishable from a guardian-authored one.
            let features_document = match &p.features {
                Some(docs) => Some(
                    fauna_protocol::encode_canonical(docs)
                        .context("encode banded feature sub-document")?
                        .to_vec(),
                ),
                None => None,
            };
            let cp = p.content_policy.unwrap_or_default();
            tx.execute(
                "INSERT INTO guardian_policies (
                     supervised_actor_id, contact_approval, unknown_sender_mail,
                     federation_contact, feed_sources,
                     content_nsfw, content_spam, content_phishing, content_commercial,
                     content_notify, unknown_peer_dm, features_document, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                rusqlite::params![
                    supervised,
                    p.contact_approval as i64,
                    p.unknown_sender_mail,
                    p.federation_contact as i64,
                    p.feed_sources,
                    cp.nsfw.as_str(),
                    cp.spam.as_str(),
                    cp.phishing.as_str(),
                    cp.commercial.as_str(),
                    p.content_notify.unwrap_or(false) as i64,
                    p.unknown_peer_dm.as_deref().unwrap_or("allow"),
                    features_document,
                    now
                ],
            )
            .context("insert banded guardian policy")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A guardian + their ward, and an unsupervised adult.
    async fn family() -> (CacheDb, [u8; 32], [u8; 32], [u8; 32]) {
        let db = CacheDb::open_in_memory().unwrap();
        let guardian = [1u8; 32];
        let ward = [2u8; 32];
        let adult = [3u8; 32];
        db.create_user_with_handle(&guardian, "personal", "parent", None)
            .await
            .unwrap();
        db.create_user_with_handle(&ward, "personal", "kid", Some(&guardian[..]))
            .await
            .unwrap();
        db.create_user_with_handle(&adult, "personal", "grownup", None)
            .await
            .unwrap();
        (db, guardian, ward, adult)
    }

    /// A deterministic Message-ID that *verifies* as Fauna-minted (the
    /// self-describing mint, `fauna_mail::msgid`) — the only ids
    /// [`CacheDb::add_sent_msgid`] seeds. Seed-determined randomness, real tag.
    fn minted(seed: u8) -> String {
        let local = fauna_mail::msgid::mint_local(&[seed; fauna_mail::msgid::MSGID_RANDOM_LEN]);
        format!("<{local}@fauna.test>")
    }

    #[tokio::test]
    async fn a_sent_msgid_correlates_only_for_the_ward_that_sent_it() {
        let (db, _guardian, ward, _adult) = family().await;
        db.add_sent_msgid(&ward[..], &minted(0xA1), 1)
            .await
            .unwrap();

        assert!(
            db.consume_sent_msgid_correlation(&ward[..], &minted(0xA1))
                .await
                .unwrap()
        );
        assert!(
            !db.consume_sent_msgid_correlation(&ward[..], &minted(0xEE))
                .await
                .unwrap(),
            "an id the ward never sent must never correlate — this is the whole gate"
        );
    }

    #[tokio::test]
    async fn msgid_matching_survives_bracket_and_case_differences() {
        // The seed reads the ward's own outbound header; the probe reads an id a
        // remote MTA copied into its report. Either may carry brackets or a
        // different case. A mismatch would over-hold real bounces, so both sides
        // route through the one shared normalizer (`fauna_mail::dedup_key`).
        let (db, _g, ward, _a) = family().await;
        let local = fauna_mail::msgid::mint_local(&[0x77; fauna_mail::msgid::MSGID_RANDOM_LEN]);
        db.add_sent_msgid(
            &ward[..],
            &format!("  <{}@Fauna.Test>  ", local.to_uppercase()),
            1,
        )
        .await
        .unwrap();
        assert!(
            db.consume_sent_msgid_correlation(&ward[..], &format!("{local}@fauna.test"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_blank_msgid_seeds_nothing_and_correlates_nothing() {
        // Fail-closed at both ends: a message with no Message-ID records no
        // correlation, and a report naming no id never matches one.
        let (db, _g, ward, _a) = family().await;
        for blank in ["", "   ", "<>"] {
            db.add_sent_msgid(&ward[..], blank, 1).await.unwrap();
            assert!(
                !db.consume_sent_msgid_correlation(&ward[..], blank)
                    .await
                    .unwrap()
            );
        }
        assert_eq!(sent_msgid_count(&db, &ward).await, 0);
    }

    #[tokio::test]
    async fn a_non_fauna_minted_msgid_is_never_seeded() {
        // A third-party MUA's id has unknowable entropy; "is it weak?" is
        // undecidable, "did a Fauna path mint it?" is not. Outside the minted
        // shape → never seeded → a report naming it is held, and the guardian
        // releases the ward's real bounces.
        let (db, _g, ward, _a) = family().await;
        for mua in [
            "<1699999999.12345@wards-laptop>",
            "<abc@fauna.test>",
            "<CADkTA4v7KXFksTFP=x1E@mail.gmail.com>",
        ] {
            db.add_sent_msgid(&ward[..], mua, 1).await.unwrap();
            assert!(
                !db.consume_sent_msgid_correlation(&ward[..], mua)
                    .await
                    .unwrap(),
                "{mua:?} must not seed a correlation"
            );
        }
        assert_eq!(sent_msgid_count(&db, &ward).await, 0);
    }

    #[tokio::test]
    async fn a_lookalike_32_hex_id_is_never_seeded() {
        // ── REGRESSION PIN ──
        //
        // 32 lowercase hex is not Fauna-distinctive: it is the exact shape of
        // an MD5 hex digest and of Python's `uuid4().hex`, both common
        // third-party Message-ID local parts — and an MUA deriving one
        // predictably (`md5(time+pid+host)` is a real pattern) makes the id
        // guessable with no thread membership at all. Shape is not
        // provenance: only an id whose embedded tag *verifies* (the
        // self-describing mint) may seed.
        let (db, _g, ward, _a) = family().await;
        for lookalike in [
            // An MD5 digest (of the empty string) as a local part.
            "<d41d8cd98f00b204e9800998ecf8427e@mail.example.com>",
            // A uuid4().hex-style local part.
            "<0f47c1a2b3d4e5f60718293a4b5c6d7e@laptop.local>",
        ] {
            db.add_sent_msgid(&ward[..], lookalike, 1).await.unwrap();
            assert!(
                !db.consume_sent_msgid_correlation(&ward[..], lookalike)
                    .await
                    .unwrap(),
                "{lookalike:?} is a shape lookalike, not a Fauna mint — it \
                 must never seed"
            );
        }
        assert_eq!(sent_msgid_count(&db, &ward).await, 0);
    }

    #[tokio::test]
    async fn the_native_mint_passes_the_seed_shape() {
        // Cross-crate pin: the compose path's actual mint must stay seedable,
        // or every native bounce would silently start holding.
        let (db, _g, ward, _a) = family().await;
        let id = fauna_conversations::rfc5322::new_message_id("fauna.test");
        db.add_sent_msgid(&ward[..], &id, 1).await.unwrap();
        assert!(
            db.consume_sent_msgid_correlation(&ward[..], &id)
                .await
                .unwrap(),
            "a real `new_message_id` mint must seed and correlate"
        );
    }

    #[tokio::test]
    async fn an_in_domain_only_message_seeds_nothing() {
        // Zero remote recipients = the message never enters the outbound queue,
        // so no remote MTA can legitimately bounce it: its id would be a
        // correlatable token with no legitimate use.
        let (db, _g, ward, _a) = family().await;
        db.add_sent_msgid(&ward[..], &minted(0xB2), 0)
            .await
            .unwrap();
        assert_eq!(sent_msgid_count(&db, &ward).await, 0);
        assert!(
            !db.consume_sent_msgid_correlation(&ward[..], &minted(0xB2))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn the_correlation_budget_is_remote_recipients_plus_two_and_never_restacks() {
        // A Message-ID leaks via `References:` to every later thread
        // participant, so a correlation is consumed, never durable. The budget
        // leaves room for every real per-recipient bounce plus a delayed/failed
        // pair; past it, a naming is a replay.
        let (db, _g, ward, _a) = family().await;
        db.add_sent_msgid(&ward[..], &minted(0xC3), 2)
            .await
            .unwrap();
        for _ in 0..4 {
            assert!(
                db.consume_sent_msgid_correlation(&ward[..], &minted(0xC3))
                    .await
                    .unwrap(),
                "2 remote recipients → 4 correlated deliveries"
            );
        }
        assert!(
            !db.consume_sent_msgid_correlation(&ward[..], &minted(0xC3))
                .await
                .unwrap(),
            "the budget is spent"
        );

        // A duplicate seed of the same id must not refill it.
        db.add_sent_msgid(&ward[..], &minted(0xC3), 2)
            .await
            .unwrap();
        assert!(
            !db.consume_sent_msgid_correlation(&ward[..], &minted(0xC3))
                .await
                .unwrap(),
            "re-seeding an existing id must never restack budget"
        );
    }

    #[tokio::test]
    async fn sent_msgids_past_the_retention_window_stop_correlating() {
        // The security window is enforced on the READ path: the seed-side prune
        // only fires when the ward next sends, so a ward who stopped sending
        // would otherwise keep old ids correlatable indefinitely. The cost of
        // the bound is that a bounce arriving after every real MTA has given up
        // (the window is 30 days; RFC 5321 puts give-up at 4–5) is *held*
        // rather than delivered — the safe direction.
        let (db, _g, ward, _a) = family().await;
        let old_id = "0123456789abcdef0123456789abcdef@fauna.test";
        let ancient = now_epoch_secs() - SENT_MSGID_RETENTION_SECS - 86_400;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO guardian_mail_sent_msgids
                     (supervised_actor_id, message_id, created_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![&ward[..], old_id, ancient],
            )
            .unwrap();
        }
        // No fresh send needed — the read path itself refuses the aged row.
        assert!(
            !db.consume_sent_msgid_correlation(&ward[..], old_id)
                .await
                .unwrap(),
            "an id past the retention window must not correlate even if unpruned"
        );

        // And a fresh send prunes it for the storage bound too.
        db.add_sent_msgid(&ward[..], &minted(0xD4), 1)
            .await
            .unwrap();
        assert_eq!(sent_msgid_count(&db, &ward).await, 1, "aged row pruned");
        assert!(
            db.consume_sent_msgid_correlation(&ward[..], &minted(0xD4))
                .await
                .unwrap(),
            "the fresh one survives its own prune"
        );
    }

    #[tokio::test]
    async fn an_unsupervised_account_stores_no_sent_msgids() {
        // The gate only exists for wards, so no adult's mail metadata is
        // retained — the same `WHERE EXISTS` no-op the allowlist has.
        let (db, _g, _w, adult) = family().await;
        db.add_sent_msgid(&adult[..], &minted(0xE5), 1)
            .await
            .unwrap();
        assert_eq!(sent_msgid_count(&db, &adult).await, 0);
        assert!(
            !db.consume_sent_msgid_correlation(&adult[..], &minted(0xE5))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn graduation_and_deletion_both_drop_the_sent_msgid_set() {
        // Graduation keeps the account and drops every trace of oversight; ward
        // deletion cascades the same rows. Neither may leave a ward's mail
        // metadata behind.
        let (db, _g, ward, _a) = family().await;
        db.add_sent_msgid(&ward[..], &minted(0xF6), 1)
            .await
            .unwrap();
        assert_eq!(sent_msgid_count(&db, &ward).await, 1);
        db.graduate(&ward[..]).await.unwrap();
        assert_eq!(sent_msgid_count(&db, &ward).await, 0);

        let (db, _g, ward2, _a) = family().await;
        db.add_sent_msgid(&ward2[..], &minted(0xF7), 1)
            .await
            .unwrap();
        db.delete_family_rows_for_supervised(&ward2[..])
            .await
            .unwrap();
        assert_eq!(sent_msgid_count(&db, &ward2).await, 0);
    }

    async fn sent_msgid_count(db: &CacheDb, ward: &[u8; 32]) -> i64 {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM guardian_mail_sent_msgids WHERE supervised_actor_id = ?1",
            rusqlite::params![&ward[..]],
            |row| row.get(0),
        )
        .unwrap()
    }

    // ── correlated-delivery origins: the reply-seed escalation break ────

    async fn origin_count(db: &CacheDb, ward: &[u8; 32]) -> i64 {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM guardian_mail_correlated_origins
             WHERE supervised_actor_id = ?1",
            rusqlite::params![&ward[..]],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn an_outbound_seed_declines_a_correlated_delivery_origin() {
        // The escalation break (§ The mail gate): the ward's reply to a
        // correlation-delivered report must not allowlist the addresses the
        // report's author chose — while the guardian's own approve, which is
        // exactly the consent the suppression preserves the need for, still
        // does.
        let (db, _g, ward, _a) = family().await;
        db.add_correlated_delivery_origins(
            &ward[..],
            &["Carol@Evil.Test".into(), "accomplice@evil.test".into()],
        )
        .await
        .unwrap();

        // The reply's auto-seed is declined (normalization included).
        db.add_mail_allowlist_entry(&ward[..], "carol@evil.test", "outbound")
            .await
            .unwrap();
        assert!(
            !db.is_known_mail_sender(&ward[..], "carol@evil.test")
                .await
                .unwrap(),
            "one correlated delivery must never buy a permanent allowlist entry"
        );

        // An unrelated correspondent the ward genuinely initiates to still
        // seeds — the suppression is per recorded address, not per ward.
        db.add_mail_allowlist_entry(&ward[..], "friend@school.test", "outbound")
            .await
            .unwrap();
        assert!(
            db.is_known_mail_sender(&ward[..], "friend@school.test")
                .await
                .unwrap()
        );

        // The guardian's explicit decision overrides the suppression.
        db.add_mail_allowlist_entry(&ward[..], "carol@evil.test", "guardian")
            .await
            .unwrap();
        assert!(
            db.is_known_mail_sender(&ward[..], "carol@evil.test")
                .await
                .unwrap(),
            "a guardian approve is consent — the suppression must yield to it"
        );
    }

    #[tokio::test]
    async fn correlated_origins_are_a_no_op_for_an_unsupervised_account() {
        // Same guard as every guardian_mail_* table: no adult's mail metadata
        // is retained, and no caller pre-checks the link.
        let (db, _g, _ward, adult) = family().await;
        db.add_correlated_delivery_origins(&adult[..], &["x@y.test".into()])
            .await
            .unwrap();
        assert_eq!(origin_count(&db, &adult).await, 0);
        // And the seed for an unsupervised account is unaffected either way.
        db.add_mail_allowlist_entry(&adult[..], "x@y.test", "outbound")
            .await
            .unwrap();
        assert!(
            !db.is_known_mail_sender(&adult[..], "x@y.test")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn an_expired_origin_no_longer_suppresses_the_seed() {
        // The suppression reads through the same 30-day window as every other
        // correlation artifact — a stale poisoned address must not gate a
        // ward's honest new correspondence forever.
        let (db, _g, ward, _a) = family().await;
        db.add_correlated_delivery_origins(&ward[..], &["old@evil.test".into()])
            .await
            .unwrap();
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE guardian_mail_correlated_origins
                 SET created_at = created_at - ?1",
                rusqlite::params![SENT_MSGID_RETENTION_SECS + 60],
            )
            .unwrap();
        }
        db.add_mail_allowlist_entry(&ward[..], "old@evil.test", "outbound")
            .await
            .unwrap();
        assert!(
            db.is_known_mail_sender(&ward[..], "old@evil.test")
                .await
                .unwrap(),
            "past the window the origin is inert"
        );
    }

    #[tokio::test]
    async fn graduation_and_deletion_both_drop_correlated_origins() {
        let (db, _g, ward, _a) = family().await;
        db.add_correlated_delivery_origins(&ward[..], &["x@evil.test".into()])
            .await
            .unwrap();
        assert_eq!(origin_count(&db, &ward).await, 1);
        db.graduate(&ward[..]).await.unwrap();
        assert_eq!(origin_count(&db, &ward).await, 0);

        let (db, _g, ward2, _a) = family().await;
        db.add_correlated_delivery_origins(&ward2[..], &["x@evil.test".into()])
            .await
            .unwrap();
        db.delete_family_rows_for_supervised(&ward2[..])
            .await
            .unwrap();
        assert_eq!(origin_count(&db, &ward2).await, 0);
    }

    // ── child-initiated contact requests' pending store ─────────────────

    #[tokio::test]
    async fn contact_requests_are_guarded_deduped_and_lazily_expired() {
        let (db, _guardian, ward, adult) = family().await;
        let peer = [9u8; 32];

        // Guarded: an unsupervised account records nothing.
        assert_eq!(
            db.add_contact_request(&adult[..], &peer[..]).await.unwrap(),
            ContactRequestAdd::NotSupervised
        );
        assert!(
            db.list_contact_requests(&adult[..])
                .await
                .unwrap()
                .is_empty()
        );

        // Created, then deduped while pending.
        let first = db.add_contact_request(&ward[..], &peer[..]).await.unwrap();
        assert!(matches!(first, ContactRequestAdd::Created { .. }));
        assert_eq!(
            db.add_contact_request(&ward[..], &peer[..]).await.unwrap(),
            ContactRequestAdd::AlreadyPending
        );
        assert_eq!(db.list_contact_requests(&ward[..]).await.unwrap().len(), 1);

        // A fresh ask after a delete gets a NEVER-reused id (AUTOINCREMENT) —
        // the doorbell dedup key rests on this.
        let ContactRequestAdd::Created { row_id: id1 } = first else {
            unreachable!()
        };
        assert!(
            db.delete_contact_request(&ward[..], &peer[..])
                .await
                .unwrap()
        );
        let ContactRequestAdd::Created { row_id: id2 } =
            db.add_contact_request(&ward[..], &peer[..]).await.unwrap()
        else {
            panic!("re-ask after delete creates");
        };
        assert_ne!(id1, id2, "a denied ask's id must never be reused");

        // Lazy expiry: a backdated row is invisible to list and delete, and a
        // re-ask then creates fresh.
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "UPDATE guardian_contact_requests SET created_at = created_at - ?1",
                rusqlite::params![PENDING_CONTACT_REQUEST_TTL_SECS + 60],
            )
            .unwrap();
        }
        assert!(
            db.list_contact_requests(&ward[..])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !db.delete_contact_request(&ward[..], &peer[..])
                .await
                .unwrap()
        );
        assert!(matches!(
            db.add_contact_request(&ward[..], &peer[..]).await.unwrap(),
            ContactRequestAdd::Created { .. }
        ));

        // The cap refuses the 33rd distinct pending ask.
        for i in 1..MAX_PENDING_CONTACT_REQUESTS {
            let mut p = [0xAAu8; 32];
            p[0] = i as u8;
            assert!(matches!(
                db.add_contact_request(&ward[..], &p[..]).await.unwrap(),
                ContactRequestAdd::Created { .. }
            ));
        }
        assert_eq!(
            db.add_contact_request(&ward[..], &[0xBBu8; 32][..])
                .await
                .unwrap(),
            ContactRequestAdd::CapExceeded
        );
    }

    // ── feed-source approvals' ask/grant store ──────────────────────────

    /// Backdate every row's pending clock by `secs` (simulating age).
    async fn age_feed_asks(db: &CacheDb, secs: i64) {
        let conn = db.conn.lock().await;
        conn.execute(
            "UPDATE guardian_feed_requests SET created_at = created_at - ?1",
            rusqlite::params![secs],
        )
        .unwrap();
    }

    /// Backdate every grant's approval clock by `secs`.
    async fn age_feed_grants(db: &CacheDb, secs: i64) {
        let conn = db.conn.lock().await;
        conn.execute(
            "UPDATE guardian_feed_requests
             SET approved_at = approved_at - ?1 WHERE approved_at IS NOT NULL",
            rusqlite::params![secs],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn feed_requests_are_guarded_deduped_and_capped() {
        let (db, _guardian, ward, adult) = family().await;

        // Guarded: an unsupervised account records nothing.
        assert_eq!(
            db.add_feed_request(&adult[..], "bluesky", "follow", "did:plc:x", "X")
                .await
                .unwrap(),
            FeedRequestAdd::NotSupervised
        );
        assert!(
            db.list_feed_requests(&adult[..], false)
                .await
                .unwrap()
                .is_empty()
        );

        // Created, then deduped while open.
        let first = db
            .add_feed_request(&ward[..], "bluesky", "follow", "did:plc:x", "X")
            .await
            .unwrap();
        assert!(matches!(first, FeedRequestAdd::Created { .. }));
        assert_eq!(
            db.add_feed_request(&ward[..], "bluesky", "follow", "did:plc:x", "X")
                .await
                .unwrap(),
            FeedRequestAdd::AlreadyOpen
        );
        assert_eq!(
            db.list_feed_requests(&ward[..], false).await.unwrap().len(),
            1
        );

        // A re-ask once the row is gone gets a NEVER-reused id (AUTOINCREMENT):
        // the guardian's doorbell dedup key rests on it, and SQLite would
        // otherwise hand back the same max rowid and swallow the re-ring.
        let FeedRequestAdd::Created { row_id: id1 } = first else {
            unreachable!()
        };
        assert!(
            db.delete_feed_request(&ward[..], "bluesky", "follow", "did:plc:x")
                .await
                .unwrap()
        );
        let FeedRequestAdd::Created { row_id: id2 } = db
            .add_feed_request(&ward[..], "bluesky", "follow", "did:plc:x", "X")
            .await
            .unwrap()
        else {
            panic!("re-ask after delete creates");
        };
        assert_ne!(id1, id2, "a denied ask's id must never be reused");

        // An already-granted row is AlreadyOpen too — the ward's next move is to
        // redeem it, not to ask again, and a re-ask must never re-ring.
        assert!(
            db.approve_feed_request(&ward[..], "bluesky", "follow", "did:plc:x")
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            db.add_feed_request(&ward[..], "bluesky", "follow", "did:plc:x", "X")
                .await
                .unwrap(),
            FeedRequestAdd::AlreadyOpen
        );

        // The cap counts asks AND unredeemed grants, and refuses the 33rd.
        for i in 1..MAX_PENDING_FEED_REQUESTS {
            assert!(
                matches!(
                    db.add_feed_request(
                        &ward[..],
                        "bluesky",
                        "follow",
                        &format!("did:plc:{i}"),
                        ""
                    )
                    .await
                    .unwrap(),
                    FeedRequestAdd::Created { .. }
                ),
                "ask {i} must be admitted"
            );
        }
        assert_eq!(
            db.add_feed_request(&ward[..], "bluesky", "follow", "did:plc:over", "")
                .await
                .unwrap(),
            FeedRequestAdd::CapExceeded
        );
    }

    /// A grant unlocks the **one** object the guardian approved: the key is the
    /// whole `(bridge, operation, target)` triple, and `label` — display-only —
    /// is outside it (`family-safety.md` § Feed-source approvals).
    #[tokio::test]
    async fn a_grant_is_single_use_and_matches_its_exact_object() {
        let (db, _g, ward, _a) = family().await;
        db.add_feed_request(&ward[..], "bluesky", "follow", "did:plc:x", "X")
            .await
            .unwrap();

        // Unapproved: nothing to spend.
        assert!(
            !db.consume_feed_grant(&ward[..], "bluesky", "follow", "did:plc:x")
                .await
                .unwrap(),
            "a pending ask is not a grant"
        );

        assert!(
            db.approve_feed_request(&ward[..], "bluesky", "follow", "did:plc:x")
                .await
                .unwrap()
                .is_some()
        );

        // Neighbouring keys do NOT match — one approved follow never unlocks a
        // different follow, a different bridge, or a different operation.
        for (b, o, t) in [
            ("bluesky", "follow", "did:plc:OTHER"),
            ("nostr", "follow", "did:plc:x"),
            ("bluesky", "feed", "did:plc:x"),
            ("bluesky", "link", ""),
        ] {
            assert!(
                !db.consume_feed_grant(&ward[..], b, o, t).await.unwrap(),
                "grant for (bluesky, follow, did:plc:x) must not unlock ({b}, {o}, {t:?})"
            );
        }

        // Single-use: the first spend fires, the second finds nothing.
        assert!(
            db.consume_feed_grant(&ward[..], "bluesky", "follow", "did:plc:x")
                .await
                .unwrap()
        );
        assert!(
            !db.consume_feed_grant(&ward[..], "bluesky", "follow", "did:plc:x")
                .await
                .unwrap(),
            "a spent grant must never fire twice"
        );
        // Spending is what removes the row, so the ward's status shows nothing.
        assert!(
            db.list_feed_requests(&ward[..], false)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Approving the *same object* twice — legitimate once the first grant has
    /// been redeemed and the ward asks again — must yield a **different** id
    /// each time, because the ward's approve-doorbell dedups on it. Every key
    /// derivable from the ask itself (the object triple, the approval second)
    /// would collide across the two decisions and silently swallow the second
    /// ring: the v23 contact-ask lesson, one table over.
    #[tokio::test]
    async fn re_approving_the_same_object_yields_a_fresh_doorbell_id() {
        let (db, _g, ward, _a) = family().await;
        db.add_feed_request(&ward[..], "bluesky", "follow", "did:plc:x", "X")
            .await
            .unwrap();
        let first = db
            .approve_feed_request(&ward[..], "bluesky", "follow", "did:plc:x")
            .await
            .unwrap()
            .expect("first approve mints a grant");

        // The ward redeems it, then asks for the same object again later.
        assert!(
            db.consume_feed_grant(&ward[..], "bluesky", "follow", "did:plc:x")
                .await
                .unwrap()
        );
        db.add_feed_request(&ward[..], "bluesky", "follow", "did:plc:x", "X")
            .await
            .unwrap();
        let second = db
            .approve_feed_request(&ward[..], "bluesky", "follow", "did:plc:x")
            .await
            .unwrap()
            .expect("second approve mints a fresh grant");

        assert_ne!(
            first, second,
            "a second approval of the same object must ring the ward again"
        );

        // A live grant is not re-approvable: a guardian double-click must not
        // silently restart the 7-day window.
        assert!(
            db.approve_feed_request(&ward[..], "bluesky", "follow", "did:plc:x")
                .await
                .unwrap()
                .is_none(),
            "an already-granted row must not be re-approvable"
        );
    }

    /// Both windows are enforced **on the read path**, not merely by the prune. Each assertion below runs on a DB where no write has happened
    /// since the row aged, so a prune-only implementation would still serve it.
    #[tokio::test]
    async fn feed_requests_windows_are_enforced_on_every_read_path() {
        // A pending ask past 30 days is invisible to list, un-approvable, and
        // un-deniable — and a re-ask then creates fresh.
        let (db, _g, ward, _a) = family().await;
        db.add_feed_request(&ward[..], "bluesky", "link", "", "")
            .await
            .unwrap();
        age_feed_asks(&db, PENDING_FEED_REQUEST_TTL_SECS + 60).await;
        assert!(
            db.list_feed_requests(&ward[..], false)
                .await
                .unwrap()
                .is_empty(),
            "an expired ask must not be listed"
        );
        assert!(
            db.approve_feed_request(&ward[..], "bluesky", "link", "")
                .await
                .unwrap()
                .is_none(),
            "an expired ask must not be approvable"
        );
        assert!(
            !db.delete_feed_request(&ward[..], "bluesky", "link", "")
                .await
                .unwrap(),
            "an expired ask must not be deniable"
        );

        // A grant past 7 days is unspendable and unlisted — the window that
        // matters most, since a lapsed grant is standing permission.
        let (db, _g, ward, _a) = family().await;
        db.add_feed_request(&ward[..], "bluesky", "feed", "at://f", "F")
            .await
            .unwrap();
        assert!(
            db.approve_feed_request(&ward[..], "bluesky", "feed", "at://f")
                .await
                .unwrap()
                .is_some()
        );
        age_feed_grants(&db, FEED_GRANT_TTL_SECS + 60).await;
        assert!(
            !db.consume_feed_grant(&ward[..], "bluesky", "feed", "at://f")
                .await
                .unwrap(),
            "a lapsed grant must be unspendable"
        );
        assert!(
            db.list_feed_requests(&ward[..], false)
                .await
                .unwrap()
                .is_empty(),
            "a lapsed grant must not be listed"
        );

        // A grant OUTLIVES the 30-day pending window: its clock runs from
        // approval, so an ask made 29 days ago and approved today is spendable.
        // (This is what a shared "created_at >= cutoff" filter would break.)
        let (db, _g, ward, _a) = family().await;
        db.add_feed_request(&ward[..], "bluesky", "feed", "at://old", "")
            .await
            .unwrap();
        age_feed_asks(&db, PENDING_FEED_REQUEST_TTL_SECS - 86_400).await;
        assert!(
            db.approve_feed_request(&ward[..], "bluesky", "feed", "at://old")
                .await
                .unwrap()
                .is_some()
        );
        age_feed_asks(&db, 2 * 86_400).await; // now well past the pending window
        assert_eq!(
            db.list_feed_requests(&ward[..], false).await.unwrap().len(),
            1,
            "a fresh grant stays live even once its ask's 30 days have passed"
        );
        assert!(
            db.consume_feed_grant(&ward[..], "bluesky", "feed", "at://old")
                .await
                .unwrap(),
            "a fresh grant is spendable regardless of when it was asked"
        );
    }

    /// The guardian's queue shows asks awaiting a decision; a granted row is
    /// awaiting the *ward's* retry, so it drops out of the queue while staying
    /// on the ward's own status read.
    #[tokio::test]
    async fn a_granted_row_leaves_the_queue_but_stays_on_the_wards_status() {
        let (db, _g, ward, _a) = family().await;
        db.add_feed_request(&ward[..], "bluesky", "follow", "did:plc:x", "X")
            .await
            .unwrap();
        assert_eq!(
            db.list_feed_requests(&ward[..], true).await.unwrap().len(),
            1
        );

        db.approve_feed_request(&ward[..], "bluesky", "follow", "did:plc:x")
            .await
            .unwrap()
            .expect("approve mints a grant");
        assert!(
            db.list_feed_requests(&ward[..], true)
                .await
                .unwrap()
                .is_empty(),
            "a granted row is no longer awaiting the guardian"
        );
        let ward_view = db.list_feed_requests(&ward[..], false).await.unwrap();
        assert_eq!(ward_view.len(), 1);
        assert!(ward_view[0].is_granted(), "the ward sees it as approved");
    }

    #[tokio::test]
    async fn graduation_and_deletion_both_drop_feed_requests() {
        let feed_count = async |db: &CacheDb, w: &[u8; 32]| -> i64 {
            let conn = db.conn.lock().await;
            conn.query_row(
                "SELECT COUNT(*) FROM guardian_feed_requests WHERE supervised_actor_id = ?1",
                rusqlite::params![&w[..]],
                |r| r.get(0),
            )
            .unwrap()
        };

        // Graduation drops both an ask and an unredeemed grant: the gate they
        // unlock no longer fires, so the ward loses nothing.
        let (db, _g, ward, _a) = family().await;
        db.add_feed_request(&ward[..], "bluesky", "link", "", "")
            .await
            .unwrap();
        db.add_feed_request(&ward[..], "bluesky", "follow", "did:plc:x", "")
            .await
            .unwrap();
        db.approve_feed_request(&ward[..], "bluesky", "follow", "did:plc:x")
            .await
            .unwrap()
            .expect("approve mints a grant");
        assert_eq!(feed_count(&db, &ward).await, 2);
        db.graduate(&ward[..]).await.unwrap();
        assert_eq!(feed_count(&db, &ward).await, 0);

        let (db, _g, ward2, _a) = family().await;
        db.add_feed_request(&ward2[..], "bluesky", "link", "", "")
            .await
            .unwrap();
        db.delete_family_rows_for_supervised(&ward2[..])
            .await
            .unwrap();
        assert_eq!(feed_count(&db, &ward2).await, 0);
    }

    // ── the bridge-DM gate's verdict set (v25) ──────────────────────────

    /// The ward's own outbound send seeds `allow` — and the seed is
    /// guardianship-guarded, so no adult's DM correspondents are ever recorded
    /// (`family-safety.md` § The bridge-DM gate).
    #[tokio::test]
    async fn the_outbound_seed_is_guarded_and_idempotent() {
        let (db, _g, ward, adult) = family().await;

        // Guarded: an unsupervised account records nothing.
        db.seed_dm_peer_allow(&adult[..], "nostr", "peer1")
            .await
            .unwrap();
        assert_eq!(
            db.dm_peer_verdict(&adult[..], "nostr", "peer1")
                .await
                .unwrap(),
            None
        );
        assert!(
            db.list_dm_peer_verdicts(&adult[..])
                .await
                .unwrap()
                .is_empty()
        );

        // A cold peer has no row — which is what makes the knob govern it.
        assert_eq!(
            db.dm_peer_verdict(&ward[..], "nostr", "peer1")
                .await
                .unwrap(),
            None
        );

        db.seed_dm_peer_allow(&ward[..], "nostr", "peer1")
            .await
            .unwrap();
        assert_eq!(
            db.dm_peer_verdict(&ward[..], "nostr", "peer1")
                .await
                .unwrap(),
            Some("allow".to_string())
        );
        // Re-sending is a no-op, not a duplicate row.
        db.seed_dm_peer_allow(&ward[..], "nostr", "peer1")
            .await
            .unwrap();
        assert_eq!(db.list_dm_peer_verdicts(&ward[..]).await.unwrap().len(), 1);
    }

    /// **The seed must never launder a guardian's block.** A ward who DMs a peer
    /// the guardian denied must not thereby re-open the conversation — this is
    /// the rule that makes the outbound refusal not the only thing standing
    /// between a ward and a blocked peer.
    #[tokio::test]
    async fn the_outbound_seed_never_overwrites_a_guardian_block() {
        let (db, _g, ward, _a) = family().await;
        assert!(
            db.set_dm_peer_verdict(&ward[..], "nostr", "peer1", DmPeerVerdict::Block)
                .await
                .unwrap()
        );

        db.seed_dm_peer_allow(&ward[..], "nostr", "peer1")
            .await
            .unwrap();

        assert_eq!(
            db.dm_peer_verdict(&ward[..], "nostr", "peer1")
                .await
                .unwrap(),
            Some("block".to_string()),
            "the ward's own send must never overwrite the guardian's block"
        );
        let rows = db.list_dm_peer_verdicts(&ward[..]).await.unwrap();
        assert_eq!(rows, vec![("nostr".into(), "peer1".into(), "block".into())]);
    }

    /// The guardian's decide *does* overwrite — it must be able to reverse an
    /// earlier decision, including their own — and is guarded like every family
    /// side table.
    #[tokio::test]
    async fn a_guardian_verdict_upserts_and_is_guarded() {
        let (db, _g, ward, adult) = family().await;

        // Guarded: unsupervised writes nothing, and reports it (the handler maps
        // this false to not_found rather than a silent ok).
        assert!(
            !db.set_dm_peer_verdict(&adult[..], "nostr", "peer1", DmPeerVerdict::Block)
                .await
                .unwrap()
        );

        assert!(
            db.set_dm_peer_verdict(&ward[..], "nostr", "peer1", DmPeerVerdict::Block)
                .await
                .unwrap()
        );
        // …and reversed by a later decide.
        assert!(
            db.set_dm_peer_verdict(&ward[..], "nostr", "peer1", DmPeerVerdict::Allow)
                .await
                .unwrap()
        );
        assert_eq!(
            db.dm_peer_verdict(&ward[..], "nostr", "peer1")
                .await
                .unwrap(),
            Some("allow".to_string())
        );
        assert_eq!(db.list_dm_peer_verdicts(&ward[..]).await.unwrap().len(), 1);
    }

    /// The verdict is keyed on `(ward, bridge, peer)` — the same peer string on
    /// two bridges is two different people.
    #[tokio::test]
    async fn verdicts_are_scoped_per_bridge_and_per_ward() {
        let (db, _g, ward, _a) = family().await;
        let other = [4u8; 32];
        db.create_user_with_handle(&other, "personal", "kid2", Some(&[1u8; 32][..]))
            .await
            .unwrap();

        db.set_dm_peer_verdict(&ward[..], "nostr", "peer1", DmPeerVerdict::Block)
            .await
            .unwrap();

        // Same peer id, different bridge → untouched.
        assert_eq!(
            db.dm_peer_verdict(&ward[..], "bluesky", "peer1")
                .await
                .unwrap(),
            None
        );
        // Same peer id, different ward → untouched.
        assert_eq!(
            db.dm_peer_verdict(&other[..], "nostr", "peer1")
                .await
                .unwrap(),
            None
        );
    }

    /// Verdicts are oversight decisions, not user content: both lifecycle ends
    /// drop them. Nothing is released because nothing was ever withheld — a held
    /// DM was only *marked*, never withheld (§ The bridge-DM gate).
    #[tokio::test]
    async fn graduation_and_deletion_both_drop_dm_verdicts() {
        let (db, _g, ward, _a) = family().await;
        db.set_dm_peer_verdict(&ward[..], "nostr", "peer1", DmPeerVerdict::Block)
            .await
            .unwrap();
        db.graduate(&ward[..]).await.unwrap();
        assert!(
            db.list_dm_peer_verdicts(&ward[..])
                .await
                .unwrap()
                .is_empty()
        );

        let (db, _g, ward2, _a) = family().await;
        db.set_dm_peer_verdict(&ward2[..], "nostr", "peer1", DmPeerVerdict::Allow)
            .await
            .unwrap();
        db.delete_family_rows_for_supervised(&ward2[..])
            .await
            .unwrap();
        assert!(
            db.list_dm_peer_verdicts(&ward2[..])
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The knob persists through the shared policy writer, and — like every
    /// v1.x field — an **absent** value leaves the stored one unchanged.
    #[tokio::test]
    async fn the_unknown_peer_dm_knob_defaults_to_allow_and_absent_leaves_it() {
        let (db, _g, ward, _a) = family().await;
        let knob = async |db: &CacheDb, w: &[u8; 32]| -> String {
            db.get_guardian_policy(&w[..])
                .await
                .unwrap()
                .unwrap()
                .unknown_peer_dm
        };

        // The default is the unsupervised-equivalent: a fresh link gates nothing.
        assert_eq!(knob(&db, &ward).await, "allow");

        db.update_guardian_policy(
            &ward[..],
            false,
            "allow",
            true,
            "allow",
            None,
            None,
            None,
            Some("hold"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(knob(&db, &ward).await, "hold");

        // A v1-era save (the knob absent) must not relax it back to allow.
        db.update_guardian_policy(
            &ward[..],
            false,
            "allow",
            true,
            "allow",
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(knob(&db, &ward).await, "hold");
    }

    // ── the transfer consent handshake's pending store ──────────────────

    #[tokio::test]
    async fn accept_repoints_the_link_only_for_the_proposed_guardian() {
        let (db, guardian, ward, adult) = family().await;
        db.upsert_pending_transfer(&ward[..], &adult[..], &guardian[..])
            .await
            .unwrap();

        // The link is untouched while the proposal pends.
        let link = db.get_guardian_of(&ward[..]).await.unwrap().unwrap();
        assert_eq!(link.guardian_actor_id, guardian.to_vec());

        // Someone the proposal does not name cannot accept.
        let intruder = [9u8; 32];
        assert!(
            !db.accept_pending_transfer(&ward[..], &intruder[..])
                .await
                .unwrap(),
            "only the proposed guardian's consent completes the transfer"
        );
        let link = db.get_guardian_of(&ward[..]).await.unwrap().unwrap();
        assert_eq!(link.guardian_actor_id, guardian.to_vec());

        // The proposed guardian's accept re-points and clears — one decision.
        assert!(
            db.accept_pending_transfer(&ward[..], &adult[..])
                .await
                .unwrap()
        );
        let link = db.get_guardian_of(&ward[..]).await.unwrap().unwrap();
        assert_eq!(link.guardian_actor_id, adult.to_vec());
        assert!(db.get_pending_transfer(&ward[..]).await.unwrap().is_none());
        // A second accept is a stale replay — nothing pending, nothing changes.
        assert!(
            !db.accept_pending_transfer(&ward[..], &adult[..])
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_new_proposal_supersedes_the_old_one() {
        let (db, guardian, ward, adult) = family().await;
        let second = [4u8; 32];
        db.upsert_pending_transfer(&ward[..], &adult[..], &guardian[..])
            .await
            .unwrap();
        db.upsert_pending_transfer(&ward[..], &second[..], &guardian[..])
            .await
            .unwrap();

        // One pending per ward: the superseded target can no longer accept.
        assert!(
            !db.accept_pending_transfer(&ward[..], &adult[..])
                .await
                .unwrap(),
            "a superseded proposal must not be acceptable"
        );
        assert_eq!(
            db.get_pending_transfer(&ward[..])
                .await
                .unwrap()
                .unwrap()
                .proposed_guardian_actor_id,
            second.to_vec()
        );
        assert_eq!(
            db.list_incoming_transfers(&adult[..]).await.unwrap().len(),
            0
        );
        assert_eq!(
            db.list_incoming_transfers(&second[..]).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn an_expired_proposal_is_invisible_and_unacceptable() {
        // Lazy expiry: past the window the row still exists physically but no
        // read returns it and no accept matches it; the next proposal write
        // prunes it. Same shape as the sent-msgid retention.
        let (db, guardian, ward, adult) = family().await;
        let stale = now_epoch_secs() - PENDING_TRANSFER_TTL_SECS - 3_600;
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO guardian_transfers
                     (supervised_actor_id, proposed_guardian_actor_id, initiated_by, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![&ward[..], &adult[..], &guardian[..], stale],
            )
            .unwrap();
        }
        assert!(db.get_pending_transfer(&ward[..]).await.unwrap().is_none());
        assert!(
            db.list_incoming_transfers(&adult[..])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !db.accept_pending_transfer(&ward[..], &adult[..])
                .await
                .unwrap(),
            "an expired proposal must not complete a transfer"
        );
        assert!(
            !db.cancel_pending_transfer(&ward[..]).await.unwrap(),
            "cancelling an expired proposal reports nothing-was-pending"
        );
        let link = db.get_guardian_of(&ward[..]).await.unwrap().unwrap();
        assert_eq!(link.guardian_actor_id, guardian.to_vec());
    }

    #[tokio::test]
    async fn decline_needs_the_named_target_and_cancel_does_not() {
        let (db, guardian, ward, adult) = family().await;
        db.upsert_pending_transfer(&ward[..], &adult[..], &guardian[..])
            .await
            .unwrap();
        let intruder = [9u8; 32];
        assert!(
            !db.decline_pending_transfer(&ward[..], &intruder[..])
                .await
                .unwrap()
        );
        assert!(
            db.decline_pending_transfer(&ward[..], &adult[..])
                .await
                .unwrap()
        );
        assert!(db.get_pending_transfer(&ward[..]).await.unwrap().is_none());

        db.upsert_pending_transfer(&ward[..], &adult[..], &guardian[..])
            .await
            .unwrap();
        assert!(db.cancel_pending_transfer(&ward[..]).await.unwrap());
        assert!(db.get_pending_transfer(&ward[..]).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn pending_transfers_cascade_with_graduation_and_with_either_party() {
        // Graduation: nothing left to transfer.
        let (db, guardian, ward, adult) = family().await;
        db.upsert_pending_transfer(&ward[..], &adult[..], &guardian[..])
            .await
            .unwrap();
        db.graduate(&ward[..]).await.unwrap();
        assert!(db.get_pending_transfer(&ward[..]).await.unwrap().is_none());

        // Ward deletion: same.
        let (db, guardian, ward, adult) = family().await;
        db.upsert_pending_transfer(&ward[..], &adult[..], &guardian[..])
            .await
            .unwrap();
        db.delete_family_rows_for_supervised(&ward[..])
            .await
            .unwrap();
        assert!(db.get_pending_transfer(&ward[..]).await.unwrap().is_none());

        // Proposed-guardian deletion: nobody left to consent.
        let (db, guardian, ward, adult) = family().await;
        db.upsert_pending_transfer(&ward[..], &adult[..], &guardian[..])
            .await
            .unwrap();
        db.delete_family_rows_for_supervised(&adult[..])
            .await
            .unwrap();
        assert!(
            db.get_pending_transfer(&ward[..]).await.unwrap().is_none(),
            "a proposal naming a deleted target must not survive them"
        );
        // The ward's own link is untouched — only the proposal died.
        let link = db.get_guardian_of(&ward[..]).await.unwrap().unwrap();
        assert_eq!(link.guardian_actor_id, guardian.to_vec());
    }
}
