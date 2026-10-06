//! Per-calendar RFC 6578 sync-token state — the shared seam that turns
//! `fauna.bridges.sync_calendar_since` into a usable "what changed since I last
//! looked" call, for all seven apps.
//!
//! # Why this exists
//!
//! `docs/goal/ui/events.md` § Where logic lives places the CalDAV delta poll in
//! shared Rust: *"Shared Rust: `sync_calendar_since` (RFC 6578) poll + a
//! calendar-change push, against the `fauna.bridges.*` RPCs."* The RPC itself
//! has been built end to end — nest handler, wire types, and the
//! [`crate::CalDavClient::sync_calendar_since`] wrapper — since Phase D.6, but
//! until this module it had **no consumer in any of the seven apps**: a grep
//! found only comments naming it as a follow-on. The missing piece was never
//! the RPC; it was the small amount of *state* a caller has to hold (one
//! sync-token per calendar) plus the paging and fall-back-to-full-read rules
//! that make the reply safe to apply.
//!
//! # What it is NOT for
//!
//! This is **not** the quick-appearance mechanism. A durable write reaches a
//! connected app over the `fauna.calendar.changed` push (built 2026-07-17), and
//! every app already consumes it; a poll on top of that is the *lossy-push
//! backstop*. So the win here is **cost, not latency**: without a delta call, a
//! backstop tick re-lists every calendar and re-fetches and unseals every event
//! in each of them. With one, a tick that finds nothing costs one round trip per
//! calendar and unseals nothing.
//!
//! # The rule that matters: a delta is only safe on top of a known baseline
//!
//! Every path that cannot honestly enumerate what changed resolves to
//! [`CalendarDelta::FullReadRequired`], never to "no changes". Silently
//! treating an unusable token as an empty delta is the one failure mode with
//! user-visible teeth — it drops events out of a calendar the user is looking
//! at, and stays wrong until something else forces a full read.

use fauna_protocol::RpcRequester;
use fauna_protocol::bridge_routing::{
    EventEntry, ExpungedEntry, SyncCalendarSinceReply, SyncCalendarSinceRequest,
};

use crate::CalDavClient;

/// Upper bound on pages consumed by one [`poll_calendar`] call.
///
/// The wire contract says a paging reply's `new_sync_token` is *"always
/// strictly greater than the input `sync_token` if `changed` is non-empty"*, so
/// a conforming nest terminates on its own. This bound is for the case that
/// promise is broken — a bug, or a nest that is not this nest — where the
/// alternative is a client that spins forever inside a backstop tick holding
/// the caller's task. Hitting it is reported as [`CalendarDelta::FullReadRequired`]
/// with [`FullReadReason::PagingDidNotConverge`] rather than as a partial delta,
/// because a truncated page set is exactly the "looks empty, isn't" shape this
/// module refuses to produce.
const MAX_PAGES: usize = 64;

/// Why a caller must fall back to a full read of the calendar instead of
/// applying a delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FullReadReason {
    /// No sync-token is held for this calendar, so there is no baseline to be
    /// incremental against. The ordinary first-poll case when the caller has
    /// not seeded from a full read.
    NoBaseline,
    /// Nest reports the held token is older than its tombstone-retention
    /// window (`Ok { stale: true }`): the *changed* set would be honest but the
    /// *deletion* set cannot be, so applying it would leave deleted events on
    /// screen.
    RetentionGap,
    /// Nest reports the held token is **ahead** of its own highestmodseq
    /// (`Stale { server_modseq }`) — the post-restore "client ahead" case.
    TokenAheadOfServer {
        /// The nest's current highestmodseq, as reported.
        server_modseq: i64,
    },
    /// Paging did not converge within [`MAX_PAGES`]. See that constant.
    PagingDidNotConverge,
    /// Nest answered with an outcome this build does not know
    /// ([`SyncCalendarSinceReply::Unknown`]): no delta can be trusted, and
    /// applying none of its deletions is the only safe reading.
    UnknownOutcome,
}

/// What one [`poll_calendar`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum CalendarDelta {
    /// The delta applies cleanly on top of the caller's current state.
    ///
    /// `changed` and `expunged` are accumulated across every page, in the
    /// `modseq ASC` order the wire promises. Both empty is the steady-state
    /// backstop answer: nothing moved, and nothing was unsealed to find out.
    Changes {
        /// Events created or updated since the held token.
        changed: Vec<EventEntry>,
        /// Deletion tombstones written since the held token.
        expunged: Vec<ExpungedEntry>,
    },
    /// The caller must re-read this calendar in full and re-seed its token via
    /// [`CalendarSyncTokens::seed`]. The stored token has already been dropped,
    /// so a caller that ignores this and polls again gets `NoBaseline` rather
    /// than a silently-wrong delta.
    FullReadRequired(FullReadReason),
    /// Nest holds no calendar row for this `(actor_id, calendar_id)` — it was
    /// deleted, or never existed. The stored token is dropped.
    CalendarNotFound,
}

/// One sync-token per calendar.
///
/// Deliberately a plain in-memory map with no persistence seam: a lost token
/// costs exactly one full read (the [`FullReadReason::NoBaseline`] path the
/// caller already has to implement), so durability buys nothing that is not
/// already handled, and every app persisting it would be seven copies of a
/// decision none of them needs to make.
#[derive(Debug, Default, Clone)]
pub struct CalendarSyncTokens {
    /// `(calendar_id, token)`. A `Vec` rather than a map: a client holds a
    /// handful of calendars, so a linear scan beats a hasher, and it keeps the
    /// type `Default` + `Clone` with no key-wrapping for the `Vec<u8>` id.
    tokens: Vec<(Vec<u8>, String)>,
}

impl CalendarSyncTokens {
    /// Empty state — every calendar will report [`FullReadReason::NoBaseline`]
    /// on its first poll.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the baseline a **full read** established.
    ///
    /// `highestmodseq` is the field `fauna.bridges.query_events` already
    /// returns, so an app that has just loaded a calendar the ordinary way can
    /// seed from it and skip the redundant first full delta. This is the
    /// intended pairing: full read once, deltas thereafter.
    pub fn seed(&mut self, calendar_id: &[u8], highestmodseq: i64) {
        self.set(calendar_id, highestmodseq.to_string());
    }

    /// The token held for `calendar_id`, if any.
    pub fn token(&self, calendar_id: &[u8]) -> Option<&str> {
        self.tokens
            .iter()
            .find(|(id, _)| id == calendar_id)
            .map(|(_, t)| t.as_str())
    }

    /// Drop the token for `calendar_id` — the next poll reports
    /// [`FullReadReason::NoBaseline`].
    pub fn forget(&mut self, calendar_id: &[u8]) {
        self.tokens.retain(|(id, _)| id != calendar_id);
    }

    /// Store a token verbatim — the write-back half of [`poll_calendar_from`]
    /// for a caller managing its own state behind a lock (the `&mut` wrapper
    /// [`poll_calendar`] uses this internally).
    pub fn set_token(&mut self, calendar_id: &[u8], token: String) {
        self.set(calendar_id, token)
    }

    fn set(&mut self, calendar_id: &[u8], token: String) {
        match self.tokens.iter_mut().find(|(id, _)| id == calendar_id) {
            Some(slot) => slot.1 = token,
            None => self.tokens.push((calendar_id.to_vec(), token)),
        }
    }
}

/// Poll one calendar for everything that changed since its held token,
/// advancing that token on success.
///
/// Returns [`CalendarDelta::FullReadRequired`] — never an empty delta — for
/// every condition under which the reply cannot be applied incrementally; see
/// [`FullReadReason`]. The token is dropped on each of those paths, so the
/// state and the returned instruction cannot disagree.
///
/// `limit` bounds the events per page (`0` = unbounded, the wire default);
/// tombstones are never paginated, so a bounded caller still sees every
/// deletion in the first reply.
pub async fn poll_calendar<R: RpcRequester>(
    client: &CalDavClient<R>,
    tokens: &mut CalendarSyncTokens,
    actor_id: &[u8; 32],
    calendar_id: &[u8],
    limit: u32,
) -> Result<CalendarDelta, R::Error> {
    let held = tokens.token(calendar_id).map(str::to_string);
    let polled = poll_calendar_from(client, actor_id, calendar_id, held.as_deref(), limit).await?;
    match polled.next_token {
        Some(t) => tokens.set(calendar_id, t),
        None => tokens.forget(calendar_id),
    }
    Ok(polled.delta)
}

/// What [`poll_calendar_from`] found, plus the token the caller should now hold.
#[derive(Debug, Clone, PartialEq)]
pub struct Polled {
    /// The outcome, exactly as [`poll_calendar`] would report it.
    pub delta: CalendarDelta,
    /// The token to store for this calendar, or `None` to drop it — which is
    /// what every [`CalendarDelta::FullReadRequired`] and
    /// [`CalendarDelta::CalendarNotFound`] answer returns, so a caller that
    /// simply writes this back cannot leave a token that outlived its baseline.
    pub next_token: Option<String>,
}

/// [`poll_calendar`] without the state container: takes the held token and
/// hands back the next one, holding no borrow of the caller's state.
///
/// This is the entry point for a caller whose token state lives behind a lock.
/// `poll_calendar` borrows `&mut CalendarSyncTokens` for the whole call, so a
/// caller holding a `std::sync::MutexGuard` to satisfy that borrow would hold
/// it across the awaits inside — which makes the resulting future `!Send` and
/// so unspawnable on a multi-threaded runtime (exactly what the linux Events
/// poll needs to do). Splitting the pure poll out lets that caller lock twice,
/// briefly, around an await it no longer spans.
pub async fn poll_calendar_from<R: RpcRequester>(
    client: &CalDavClient<R>,
    actor_id: &[u8; 32],
    calendar_id: &[u8],
    token: Option<&str>,
    limit: u32,
) -> Result<Polled, R::Error> {
    let Some(start) = token else {
        return Ok(Polled {
            delta: CalendarDelta::FullReadRequired(FullReadReason::NoBaseline),
            next_token: None,
        });
    };

    let mut cursor = start.to_string();
    let mut changed: Vec<EventEntry> = Vec::new();
    let mut expunged: Vec<ExpungedEntry> = Vec::new();

    for _ in 0..MAX_PAGES {
        let reply = client
            .sync_calendar_since(SyncCalendarSinceRequest {
                actor_id: actor_id.to_vec(),
                calendar_id: calendar_id.to_vec(),
                sync_token: cursor.clone(),
                limit,
                mua_id: None,
            })
            .await?;

        match reply {
            SyncCalendarSinceReply::CalendarNotFound => {
                return Ok(Polled {
                    delta: CalendarDelta::CalendarNotFound,
                    next_token: None,
                });
            }
            SyncCalendarSinceReply::Stale { server_modseq } => {
                return Ok(Polled {
                    delta: CalendarDelta::FullReadRequired(FullReadReason::TokenAheadOfServer {
                        server_modseq,
                    }),
                    next_token: None,
                });
            }
            SyncCalendarSinceReply::Unknown => {
                return Ok(Polled {
                    delta: CalendarDelta::FullReadRequired(FullReadReason::UnknownOutcome),
                    next_token: None,
                });
            }
            SyncCalendarSinceReply::Ok { stale: true, .. } => {
                // The changed set would be honest here, but the deletion set
                // cannot be — so applying it would leave deleted events on
                // screen. Discard what this call accumulated: a partial apply
                // is the failure mode, not a saving.
                return Ok(Polled {
                    delta: CalendarDelta::FullReadRequired(FullReadReason::RetentionGap),
                    next_token: None,
                });
            }
            SyncCalendarSinceReply::Ok {
                changed: page,
                expunged: gone,
                new_sync_token,
                more,
                stale: false,
            } => {
                changed.extend(page);
                expunged.extend(gone);
                cursor = new_sync_token;
                if !more {
                    return Ok(Polled {
                        delta: CalendarDelta::Changes { changed, expunged },
                        next_token: Some(cursor),
                    });
                }
            }
        }
    }

    Ok(Polled {
        delta: CalendarDelta::FullReadRequired(FullReadReason::PagingDidNotConverge),
        next_token: None,
    })
}

/// What one backstop tick should do about a single calendar.
///
/// Deliberately two arms and no error arm: see [`backstop_probe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackstopVerdict {
    /// Nest enumerated nothing since the held token: skip the full read
    /// entirely. Store `next_token` (when `Some`) as this calendar's new
    /// baseline so the next tick stays incremental.
    Unchanged { next_token: Option<String> },
    /// Read the calendar in full and re-seed its token from that read
    /// (`query_events_in_seeded`, or the caller's equivalent). Every condition
    /// that is not a clean empty delta lands here — including the ones that
    /// are not errors at all.
    ReadRequired,
}

/// Should this backstop tick pay for a full read of `calendar_id`?
///
/// This is the whole decision an Events-page backstop poll makes per calendar,
/// as one call: hold no baseline → read (and do not even ask); ask, and skip
/// the read only on a clean empty delta; anything else → read. Every app owes
/// exactly this decision, so it lives here rather than seven times over — the
/// RPC and the token bookkeeping were already shared, and re-deriving the
/// *rule* per app is where a hand-copy goes wrong.
///
/// Three properties the type enforces, each the fail-open a call site would
/// otherwise have to remember:
///
/// * **A transport error is [`BackstopVerdict::ReadRequired`], never an
///   absence of changes.** There is no error arm, and the function returns no
///   `Result`, so `if let Ok(reply) = …` — whose implicit `else` silently
///   means "nothing changed", freezing the page for as long as the transport
///   is down — is not expressible here.
/// * **Every [`FullReadReason`] collapses to the same instruction**, so a
///   caller cannot forget the arm for a reason added later. That is also why
///   the reason is not surfaced: nothing a caller does with it differs.
/// * **The delta is a change *detector* only** — the changed and expunged sets
///   are deliberately not returned. What renders is therefore always something
///   nest just sent in full, so a mis-applied delta cannot silently drop an
///   event out of a calendar the user is looking at. Applying deltas
///   incrementally is a further step; it would need a different seam, not a
///   richer verdict from this one.
///
/// The held token is passed in and the next one handed back — rather than this
/// taking `&mut CalendarSyncTokens` — for [`poll_calendar_from`]'s reason: a
/// caller whose token map lives behind a `std::sync::Mutex` can lock twice,
/// briefly, around a call it no longer spans, and keep the future `Send`.
///
/// ```ignore
/// let held = tokens.lock()?.token(&cal).map(str::to_string);
/// match backstop_probe(&client, &actor, &cal, held.as_deref(), 0).await {
///     BackstopVerdict::Unchanged { next_token } => {
///         if let Some(t) = next_token {
///             tokens.lock()?.set_token(&cal, t);
///         }
///         return; // the UI already holds a full list nest just called current
///     }
///     BackstopVerdict::ReadRequired => { /* fall through to the full read */ }
/// }
/// ```
///
/// Skipping is safe because of a one-way invariant the caller must preserve: a
/// token exists for a calendar ONLY if a full read of it completed, and that
/// read published its result. So "we hold a token" implies "the UI already has
/// a full event list for this calendar", and skipping leaves it displaying a
/// list nest has just confirmed is still current.
pub async fn backstop_probe<R: RpcRequester>(
    client: &CalDavClient<R>,
    actor_id: &[u8; 32],
    calendar_id: &[u8],
    held_token: Option<&str>,
    limit: u32,
) -> BackstopVerdict {
    if held_token.is_none() {
        // `poll_calendar_from` would answer `NoBaseline` without a round trip
        // too; short-circuiting here as well keeps that guarantee a property of
        // *this* function rather than an inherited implementation detail.
        return BackstopVerdict::ReadRequired;
    }

    let Ok(polled) = poll_calendar_from(client, actor_id, calendar_id, held_token, limit).await
    else {
        return BackstopVerdict::ReadRequired;
    };

    match polled.delta {
        CalendarDelta::Changes { changed, expunged }
            if changed.is_empty() && expunged.is_empty() =>
        {
            BackstopVerdict::Unchanged {
                next_token: polled.next_token,
            }
        }
        _ => BackstopVerdict::ReadRequired,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{FailingRequester, ScriptedRequester, block_on};
    use std::sync::Arc;

    const ACTOR: [u8; 32] = [7u8; 32];
    const CAL: [u8; 32] = [9u8; 32];

    fn event(modseq: i64) -> EventEntry {
        EventEntry {
            event_id: vec![modseq as u8; 32],
            uid_hash: vec![modseq as u8; 32],
            encrypted_body: vec![1, 2, 3],
            encrypted_index_hint: vec![],
            etag: format!("etag-{modseq}"),
            modseq,
            ciphertext_size: 3,
            internal_date: 0,
            encrypted_fauna_ext: None,
        }
    }

    fn tombstone(modseq: i64) -> ExpungedEntry {
        ExpungedEntry {
            event_id: vec![modseq as u8; 32],
            uid_hash: vec![modseq as u8; 32],
            modseq,
        }
    }

    fn enc(reply: &SyncCalendarSinceReply) -> Vec<u8> {
        fauna_protocol::encode_canonical(reply)
            .expect("encode reply")
            .to_vec()
    }

    fn ok(
        changed: Vec<EventEntry>,
        expunged: Vec<ExpungedEntry>,
        token: &str,
        more: bool,
    ) -> Vec<u8> {
        enc(&SyncCalendarSinceReply::Ok {
            changed,
            expunged,
            new_sync_token: token.into(),
            more,
            stale: false,
        })
    }

    fn poll(
        replies: Vec<Vec<u8>>,
        tokens: &mut CalendarSyncTokens,
    ) -> (CalendarDelta, Arc<ScriptedRequester>) {
        let req = Arc::new(ScriptedRequester::new(replies));
        let client = CalDavClient::new(Arc::clone(&req));
        let out = block_on(poll_calendar(&client, tokens, &ACTOR, &CAL, 0)).expect("infallible");
        (out, req)
    }

    /// The baseline rule: with no token held there is nothing to be
    /// incremental against, so the caller is told to read fully — and, the part
    /// that matters, **no RPC is sent**. A poll that asked nest with token "0"
    /// would get the whole calendar back and look like a working delta.
    #[test]
    fn a_calendar_with_no_token_demands_a_full_read_without_calling_nest() {
        let mut tokens = CalendarSyncTokens::new();
        let (out, req) = poll(vec![], &mut tokens);
        assert_eq!(
            out,
            CalendarDelta::FullReadRequired(FullReadReason::NoBaseline)
        );
        assert!(
            req.kinds().is_empty(),
            "no round trip on the no-baseline path"
        );
    }

    /// Seeding from a full read's `highestmodseq` is what makes the first poll
    /// incremental, and the token is sent verbatim as the wire's decimal string.
    #[test]
    fn a_seeded_token_is_sent_as_the_sync_token() {
        let mut tokens = CalendarSyncTokens::new();
        tokens.seed(&CAL, 41);
        let (out, req) = poll(vec![ok(vec![], vec![], "41", false)], &mut tokens);
        assert_eq!(
            out,
            CalendarDelta::Changes {
                changed: vec![],
                expunged: vec![]
            }
        );
        assert_eq!(req.kinds(), vec!["fauna.bridges.sync_calendar_since"]);
        let sent: SyncCalendarSinceRequest =
            fauna_protocol::decode_strict(&req.payloads()[0]).expect("decode request");
        assert_eq!(sent.sync_token, "41");
        assert_eq!(sent.calendar_id, CAL.to_vec());
        assert_eq!(sent.actor_id, ACTOR.to_vec());
    }

    /// The steady-state backstop tick: one round trip, nothing returned, and
    /// the token advances to the calendar-wide highestmodseq.
    #[test]
    fn a_quiet_calendar_costs_one_round_trip_and_advances_the_token() {
        let mut tokens = CalendarSyncTokens::new();
        tokens.seed(&CAL, 41);
        let (out, req) = poll(vec![ok(vec![], vec![], "55", false)], &mut tokens);
        assert_eq!(
            out,
            CalendarDelta::Changes {
                changed: vec![],
                expunged: vec![]
            }
        );
        assert_eq!(req.kinds().len(), 1);
        assert_eq!(tokens.token(&CAL), Some("55"));
    }

    /// Paging: `more: true` means call again with the returned token, and the
    /// accumulated result is every page's events in order. This is the case a
    /// reply-table double cannot express at all — it would answer page one
    /// forever.
    #[test]
    fn more_pages_forward_carrying_the_returned_token_and_accumulates() {
        let mut tokens = CalendarSyncTokens::new();
        tokens.seed(&CAL, 0);
        let (out, req) = poll(
            vec![
                ok(vec![event(1), event(2)], vec![tombstone(3)], "2", true),
                ok(vec![event(4)], vec![], "9", false),
            ],
            &mut tokens,
        );
        match out {
            CalendarDelta::Changes { changed, expunged } => {
                assert_eq!(
                    changed.iter().map(|e| e.modseq).collect::<Vec<_>>(),
                    vec![1, 2, 4]
                );
                assert_eq!(
                    expunged.iter().map(|e| e.modseq).collect::<Vec<_>>(),
                    vec![3]
                );
            }
            other => panic!("expected Changes, got {other:?}"),
        }
        assert_eq!(req.kinds().len(), 2, "paged exactly twice");
        let second: SyncCalendarSinceRequest =
            fauna_protocol::decode_strict(&req.payloads()[1]).expect("decode request");
        assert_eq!(
            second.sync_token, "2",
            "the second page must carry the first reply's token"
        );
        assert_eq!(tokens.token(&CAL), Some("9"), "token ends at the last page");
        assert_eq!(
            req.remaining(),
            0,
            "the loop stopped when `more` went false"
        );
    }

    /// `stale: true` means nest cannot honestly enumerate deletions since the
    /// held token. The changed set alone would leave deleted events on screen,
    /// so this must NOT read as a delta — and the token is dropped so the next
    /// poll cannot paper over it.
    #[test]
    fn a_retention_gap_demands_a_full_read_and_drops_the_token() {
        let mut tokens = CalendarSyncTokens::new();
        tokens.seed(&CAL, 41);
        let (out, _) = poll(
            vec![enc(&SyncCalendarSinceReply::Ok {
                changed: vec![event(42)],
                expunged: vec![],
                new_sync_token: "42".into(),
                more: false,
                stale: true,
            })],
            &mut tokens,
        );
        assert_eq!(
            out,
            CalendarDelta::FullReadRequired(FullReadReason::RetentionGap)
        );
        assert_eq!(tokens.token(&CAL), None, "token dropped, not advanced");
    }

    /// The post-restore "client ahead of server" case is reported with the
    /// server's own modseq, and likewise drops the token.
    #[test]
    fn a_token_ahead_of_the_server_demands_a_full_read() {
        let mut tokens = CalendarSyncTokens::new();
        tokens.seed(&CAL, 900);
        let (out, _) = poll(
            vec![enc(&SyncCalendarSinceReply::Stale { server_modseq: 12 })],
            &mut tokens,
        );
        assert_eq!(
            out,
            CalendarDelta::FullReadRequired(FullReadReason::TokenAheadOfServer {
                server_modseq: 12
            })
        );
        assert_eq!(tokens.token(&CAL), None);
    }

    /// A calendar that no longer exists is its own outcome — distinct from
    /// "nothing changed", which is what would strand a deleted calendar's
    /// events on screen forever.
    #[test]
    fn a_missing_calendar_is_reported_as_such() {
        let mut tokens = CalendarSyncTokens::new();
        tokens.seed(&CAL, 41);
        let (out, _) = poll(
            vec![enc(&SyncCalendarSinceReply::CalendarNotFound)],
            &mut tokens,
        );
        assert_eq!(out, CalendarDelta::CalendarNotFound);
        assert_eq!(tokens.token(&CAL), None);
    }

    /// A newer nest answering with an outcome this build cannot name (modelled
    /// by a twin enum that carries one extra outcome) must read as a full-read
    /// instruction with the token dropped — never an empty delta, never
    /// calendar-not-found, so no deletion is applied.
    #[test]
    fn an_outcome_from_a_newer_nest_demands_a_full_read() {
        #[derive(serde::Serialize)]
        #[serde(tag = "outcome", rename_all = "snake_case")]
        enum NewerSyncReply {
            FromTheFuture,
        }
        let mut tokens = CalendarSyncTokens::new();
        tokens.seed(&CAL, 41);
        let newer = fauna_protocol::encode_canonical(&NewerSyncReply::FromTheFuture)
            .expect("encode twin")
            .to_vec();
        let (out, _) = poll(vec![newer], &mut tokens);
        assert_eq!(
            out,
            CalendarDelta::FullReadRequired(FullReadReason::UnknownOutcome)
        );
        assert_eq!(tokens.token(&CAL), None, "token dropped, not advanced");
    }

    /// A nest that always says `more: true` must not spin the caller's task
    /// forever, and must not yield a truncated delta either — the bounded exit
    /// is a full-read instruction.
    #[test]
    fn paging_that_never_converges_is_bounded_and_demands_a_full_read() {
        let mut tokens = CalendarSyncTokens::new();
        tokens.seed(&CAL, 0);
        let replies = (0..MAX_PAGES)
            .map(|i| {
                ok(
                    vec![event(i as i64 + 1)],
                    vec![],
                    &(i + 1).to_string(),
                    true,
                )
            })
            .collect::<Vec<_>>();
        let (out, req) = poll(replies, &mut tokens);
        assert_eq!(
            out,
            CalendarDelta::FullReadRequired(FullReadReason::PagingDidNotConverge)
        );
        assert_eq!(req.kinds().len(), MAX_PAGES, "stopped at the bound");
        assert_eq!(tokens.token(&CAL), None);
    }

    /// Token state is per calendar — one calendar's full-read reset must not
    /// disturb another's baseline.
    #[test]
    fn tokens_are_scoped_per_calendar() {
        let other = [3u8; 32];
        let mut tokens = CalendarSyncTokens::new();
        tokens.seed(&CAL, 41);
        tokens.seed(&other, 77);
        tokens.forget(&CAL);
        assert_eq!(tokens.token(&CAL), None);
        assert_eq!(tokens.token(&other), Some("77"));
    }

    /// The lock-friendly entry point's whole contract: it borrows no caller
    /// state, and every answer that is not an applicable delta hands back
    /// `next_token: None`. A caller that simply writes `next_token` back
    /// therefore cannot leave a token standing that outlived its baseline —
    /// which is the bug that would turn one bad reply into a permanently wrong
    /// calendar.
    #[test]
    fn the_lock_free_entry_point_hands_back_no_token_on_every_full_read_path() {
        for reply in [
            enc(&SyncCalendarSinceReply::Stale { server_modseq: 3 }),
            enc(&SyncCalendarSinceReply::CalendarNotFound),
            enc(&SyncCalendarSinceReply::Ok {
                changed: vec![event(5)],
                expunged: vec![],
                new_sync_token: "5".into(),
                more: false,
                stale: true,
            }),
        ] {
            let req = Arc::new(ScriptedRequester::new(vec![reply]));
            let client = CalDavClient::new(Arc::clone(&req));
            let polled = block_on(poll_calendar_from(&client, &ACTOR, &CAL, Some("1"), 0))
                .expect("infallible");
            assert!(
                matches!(
                    polled.delta,
                    CalendarDelta::FullReadRequired(_) | CalendarDelta::CalendarNotFound
                ),
                "expected a full-read answer, got {:?}",
                polled.delta
            );
            assert_eq!(polled.next_token, None, "no token may survive: {polled:?}");
        }
    }

    /// And the applicable-delta path does hand one back, so the wrapper has
    /// something to store.
    #[test]
    fn the_lock_free_entry_point_hands_back_the_new_token_on_a_clean_delta() {
        let req = Arc::new(ScriptedRequester::new(vec![ok(
            vec![],
            vec![],
            "88",
            false,
        )]));
        let client = CalDavClient::new(Arc::clone(&req));
        let polled =
            block_on(poll_calendar_from(&client, &ACTOR, &CAL, Some("1"), 0)).expect("infallible");
        assert_eq!(polled.next_token.as_deref(), Some("88"));
    }

    /// Re-seeding replaces rather than appends — otherwise a long-lived client
    /// would grow a second, stale entry per calendar per full read, and
    /// `token()`'s first-match lookup would keep answering with the oldest.
    #[test]
    fn re_seeding_a_calendar_replaces_its_token() {
        let mut tokens = CalendarSyncTokens::new();
        tokens.seed(&CAL, 41);
        tokens.seed(&CAL, 99);
        assert_eq!(tokens.token(&CAL), Some("99"));
    }

    // ---------------------------------------------------------------------
    // `backstop_probe` — the skip-or-read decision itself
    // ---------------------------------------------------------------------

    fn probe(
        replies: Vec<Vec<u8>>,
        held: Option<&str>,
    ) -> (BackstopVerdict, Arc<ScriptedRequester>) {
        let req = Arc::new(ScriptedRequester::new(replies));
        let client = CalDavClient::new(Arc::clone(&req));
        let verdict = block_on(backstop_probe(&client, &ACTOR, &CAL, held, 0));
        (verdict, req)
    }

    /// The saving this whole seam exists for, and the ONLY shape that may
    /// claim it: nest answered, converged, and enumerated nothing at all.
    #[test]
    fn a_clean_empty_delta_is_the_one_verdict_that_skips_the_read() {
        let (verdict, _) = probe(vec![ok(vec![], vec![], "42", false)], Some("41"));
        assert_eq!(
            verdict,
            BackstopVerdict::Unchanged {
                next_token: Some("42".into())
            }
        );
    }

    /// With no baseline there is nothing to be incremental against, and the
    /// round trip's answer is a foregone `NoBaseline` — so the probe must cost
    /// nothing. A caller that asked anyway would pay a full extra RPC per
    /// calendar per tick for the entire first pass over a cold cache.
    #[test]
    fn an_unseeded_calendar_demands_a_read_without_calling_nest() {
        let (verdict, req) = probe(vec![], None);
        assert_eq!(verdict, BackstopVerdict::ReadRequired);
        assert!(req.kinds().is_empty(), "no round trip without a baseline");
    }

    /// A changed event is a change: the caller re-reads. (This seam uses the
    /// delta as a *detector* and never applies it, so "what changed" is
    /// deliberately not part of the verdict — see the fn's own docs.)
    #[test]
    fn a_changed_event_demands_the_read() {
        let (verdict, _) = probe(vec![ok(vec![event(42)], vec![], "42", false)], Some("41"));
        assert_eq!(verdict, BackstopVerdict::ReadRequired);
    }

    /// A deletion with no accompanying change is still a change. Asserted
    /// separately from the arm above because the two sets are independent
    /// fields, and a predicate testing only `changed.is_empty()` passes every
    /// other test in this file while silently leaving deleted events on screen.
    #[test]
    fn a_tombstone_alone_demands_the_read() {
        let (verdict, _) = probe(
            vec![ok(vec![], vec![tombstone(42)], "42", false)],
            Some("41"),
        );
        assert_eq!(verdict, BackstopVerdict::ReadRequired);
    }

    /// Every `FullReadRequired` reason collapses to the same instruction, so
    /// the caller needs no arm per reason and cannot forget one. Pinned on the
    /// retention gap because it is the reason whose reply *carries* an honest
    /// changed set — the tempting one to treat as a usable delta.
    #[test]
    fn a_retention_gap_demands_the_read() {
        let stale = enc(&SyncCalendarSinceReply::Ok {
            changed: vec![],
            expunged: vec![],
            new_sync_token: "42".into(),
            more: false,
            stale: true,
        });
        let (verdict, _) = probe(vec![stale], Some("41"));
        assert_eq!(verdict, BackstopVerdict::ReadRequired);
    }

    /// A calendar that vanished server-side is not an unchanged calendar.
    #[test]
    fn a_vanished_calendar_demands_the_read() {
        let (verdict, _) = probe(
            vec![enc(&SyncCalendarSinceReply::CalendarNotFound)],
            Some("41"),
        );
        assert_eq!(verdict, BackstopVerdict::ReadRequired);
    }

    /// **The fail-safe.** A round trip that did not happen answers nothing, and
    /// the one answer it must never be taken for is "nothing changed" — that
    /// reads a dropped connection as a quiet calendar and freezes the page for
    /// as long as the transport stays down. The verdict type has no error arm
    /// precisely so this cannot be got wrong at a call site: the failure IS
    /// `ReadRequired`.
    #[test]
    fn a_transport_error_never_reads_as_unchanged() {
        let req = Arc::new(FailingRequester::new("socket closed"));
        let client = CalDavClient::new(Arc::clone(&req));
        let verdict = block_on(backstop_probe(&client, &ACTOR, &CAL, Some("41"), 0));
        assert_eq!(verdict, BackstopVerdict::ReadRequired);
        assert_eq!(
            req.kinds(),
            vec!["fauna.bridges.sync_calendar_since"],
            "it did try before falling back"
        );
    }

    /// The token advances across a skipped read, so a run of quiet ticks stays
    /// incremental instead of silently re-asking from the same old baseline
    /// (which still answers "nothing changed", so no test of the *verdict*
    /// alone can catch it).
    #[test]
    fn a_skipped_read_still_advances_the_baseline() {
        let (verdict, _) = probe(vec![ok(vec![], vec![], "99", false)], Some("41"));
        let BackstopVerdict::Unchanged { next_token } = verdict else {
            panic!("expected Unchanged");
        };
        assert_eq!(next_token.as_deref(), Some("99"));
    }
}
