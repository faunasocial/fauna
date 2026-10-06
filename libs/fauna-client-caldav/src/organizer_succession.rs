//! The per-session memo over the inbound rule's succession lookup
//! (`caldav-server.md` § Who may mutate an existing event over the inbound rail
//! → *A succeeded organizer*).
//!
//! The lookup is a dial and a walk at the event's bound nest, and it runs
//! inline in the account's one receive loop. Unremembered, every stale-organizer
//! `CANCEL` or updating `REQUEST` would pay it again, so a slow or hostile bound
//! nest could hold the loop for the whole round-trip budget per message it
//! sends. [`MemoizedSuccessionResolver`] is the one implementation both shipped
//! sinks use — native in `fauna-client-conversations`, web in `fauna-wasm` —
//! and each supplies only its platform's dial ([`SuccessionDialer`]). The rule
//! is the in-group succession witness's (`fauna_client_recovery::witness`):
//! one dial per bound identity per session, a dial that settled nothing is
//! remembered too, and a lookup that made no dial is never remembered. One
//! narrowing of the witness's rule: a bound nest's definitive "never
//! succeeded" is re-asked rather than remembered, since anyone can make the
//! victim ask it before a succession lands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::{PrincipalResolver, SchedulingPrincipal};

/// What one succession lookup at a bound nest concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuccessionLookup {
    /// The walk verified: the bound identity ended up at this actor (64-hex).
    Succeeded(String),
    /// A dial was made and settled nothing: the anchor would not dial, timed
    /// out, or failed the walk — a transport error, or a chain that fails the
    /// rule. Remembered for the session, which is the whole point of the memo:
    /// this is the answer that costs the round-trip budget. A chain that fails
    /// the rule is remembered with the unreachable class rather than re-asked:
    /// an honest bound nest never serves one, so no stranger can pin it, and
    /// asking a nest that does again buys nothing. A relaunch asks again.
    Unproven,
    /// The bound nest answered, and its answer is that the identity has not
    /// succeeded. **Never remembered**: it is
    /// true only *so far* — any account on that nest can make the recipient
    /// ask it with one message before a succession lands, and remembering it
    /// would refuse the successor's genuine `CANCEL` or update for the rest of
    /// the session, terminally, since a refusal is never retried. Re-asking a
    /// nest that answers costs nothing the rail's own per-tick fetch from that
    /// nest does not already pay.
    NotSucceeded,
    /// No dial was made — the held chain head could not be read, or the bound
    /// actor id does not parse — so nothing is known. Answers *no answer* and
    /// is **never** remembered: a later message may find the config readable,
    /// and remembering an outage that cost nothing would refuse it for no
    /// saving.
    NotAsked,
}

/// A platform's succession dial: read the held chain head for the bound
/// identity, dial `anchor_nest_url` anonymously and run the verified walk
/// there, all inside the witness's round-trip budget. wasm-safe (static
/// dispatch, no `Send` bound).
#[allow(async_fn_in_trait)] // static-dispatch only, like `PrincipalResolver`
pub trait SuccessionDialer {
    /// Where `old_actor_id` (64-hex) verifiably ended up, as seen from
    /// `anchor_nest_url` — a URL the caller recorded before any succession was
    /// asserted, never one the inbound message carries.
    async fn walk(&self, old_actor_id: &str, anchor_nest_url: &str) -> SuccessionLookup;
}

/// A memo key: the bound actor id and the anchor nest URL, both normalized.
type MemoKey = (String, String);

/// The session's remembered succession answers, keyed by `(bound actor,
/// anchor)`. A cheap handle: clones share one memo. Owned by whatever lives as
/// long as the account's receive session — the native sink, the web manager —
/// never by a resolver built per message.
#[derive(Clone, Default)]
pub struct SuccessionMemo {
    /// `Some(successor)` = verified; `None` = [`SuccessionLookup::Unproven`].
    /// Nothing else is ever stored.
    answers: Arc<Mutex<HashMap<MemoKey, Option<String>>>>,
}

impl SuccessionMemo {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(old_actor_id: &str, anchor_nest_url: &str) -> MemoKey {
        (
            old_actor_id.trim().to_ascii_lowercase(),
            anchor_nest_url
                .trim()
                .trim_end_matches('/')
                .to_ascii_lowercase(),
        )
    }

    fn get(&self, key: &MemoKey) -> Option<Option<String>> {
        self.answers
            .lock()
            .expect("lock poisoned")
            .get(key)
            .cloned()
    }

    fn remember(&self, key: MemoKey, answer: Option<String>) {
        self.answers
            .lock()
            .expect("lock poisoned")
            .insert(key, answer);
    }
}

/// The inbound rule's shipped [`PrincipalResolver`]: addresses answered by
/// `addresses` (the shared `DiscoveryPrincipalResolver` in both sinks), and a
/// succession asked of `dialer` at most once per `(bound actor, anchor)` for
/// the life of `memo`.
pub struct MemoizedSuccessionResolver<R, D> {
    /// Answers CAL-ADDRESSES; untouched by the memo.
    pub addresses: R,
    /// The platform's dial and walk.
    pub dialer: D,
    /// The recipient's own nest — the anchor of a blank binding.
    pub own_nest_url: String,
    /// The session's remembered answers.
    pub memo: SuccessionMemo,
}

impl<R: PrincipalResolver, D: SuccessionDialer> PrincipalResolver
    for MemoizedSuccessionResolver<R, D>
{
    async fn resolve_principal(&self, caladdr: &str) -> Option<SchedulingPrincipal> {
        self.addresses.resolve_principal(caladdr).await
    }

    async fn resolve_successor(&self, bound: &SchedulingPrincipal) -> Option<String> {
        // The anchor is the BOUND nest — recorded by this client when the event
        // was created, long before anyone asserted a succession — never a URL
        // the inbound message carries. Blank = this account's own nest.
        let anchor = if bound.home_nest_url.trim().is_empty() {
            self.own_nest_url.as_str()
        } else {
            bound.home_nest_url.as_str()
        };
        let key = SuccessionMemo::key(&bound.actor_id, anchor);
        if let Some(answer) = self.memo.get(&key) {
            return answer;
        }
        // Two messages for one identity landing concurrently may both dial;
        // the receive loop applies one message at a time, so that does not
        // arise on the path this memo bounds.
        match self.dialer.walk(&bound.actor_id, anchor).await {
            SuccessionLookup::Succeeded(successor) => {
                self.memo.remember(key, Some(successor.clone()));
                Some(successor)
            }
            SuccessionLookup::Unproven => {
                self.memo.remember(key, None);
                None
            }
            SuccessionLookup::NotSucceeded | SuccessionLookup::NotAsked => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use fauna_client_testkit::block_on;

    use super::*;
    use crate::NoPrincipalResolver;

    const OLD: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
    const OTHER: &str = "bb00000000000000000000000000000000000000000000000000000000000000";
    const NEW: &str = "cc00000000000000000000000000000000000000000000000000000000000000";
    const OWN: &str = "https://own.example";

    /// Counts dials and answers every one the same way, recording each anchor.
    struct CountingDialer {
        answer: SuccessionLookup,
        dials: AtomicUsize,
        anchors: Mutex<Vec<String>>,
    }

    impl CountingDialer {
        fn answering(answer: SuccessionLookup) -> Self {
            Self {
                answer,
                dials: AtomicUsize::new(0),
                anchors: Mutex::new(Vec::new()),
            }
        }
    }

    impl SuccessionDialer for &CountingDialer {
        async fn walk(&self, _old: &str, anchor_nest_url: &str) -> SuccessionLookup {
            self.dials.fetch_add(1, Ordering::SeqCst);
            self.anchors
                .lock()
                .unwrap()
                .push(anchor_nest_url.to_string());
            self.answer.clone()
        }
    }

    fn resolver<'d>(
        dialer: &'d CountingDialer,
        memo: &SuccessionMemo,
    ) -> MemoizedSuccessionResolver<NoPrincipalResolver, &'d CountingDialer> {
        MemoizedSuccessionResolver {
            addresses: NoPrincipalResolver,
            dialer,
            own_nest_url: OWN.into(),
            memo: memo.clone(),
        }
    }

    fn bound(actor: &str, home: &str) -> SchedulingPrincipal {
        SchedulingPrincipal {
            actor_id: actor.into(),
            home_nest_url: home.into(),
        }
    }

    /// A verified answer is remembered: the second stale-organizer message for
    /// the same bound identity costs no dial — even through a resolver built
    /// afresh, as both sinks build one per message or per drain.
    #[test]
    fn a_verified_succession_is_dialed_once_per_session() {
        let dialer = CountingDialer::answering(SuccessionLookup::Succeeded(NEW.into()));
        let memo = SuccessionMemo::new();
        for _ in 0..2 {
            let answer = block_on(resolver(&dialer, &memo).resolve_successor(&bound(OLD, "")));
            assert_eq!(answer.as_deref(), Some(NEW));
        }
        assert_eq!(dialer.dials.load(Ordering::SeqCst), 1);
    }

    /// A dial that settled nothing is remembered too — this is the case a slow
    /// or hostile bound nest forces, and the one the memo exists to bound.
    #[test]
    fn an_unproven_succession_is_dialed_once_per_session() {
        let dialer = CountingDialer::answering(SuccessionLookup::Unproven);
        let memo = SuccessionMemo::new();
        for _ in 0..3 {
            assert_eq!(
                block_on(resolver(&dialer, &memo).resolve_successor(&bound(OLD, ""))),
                None
            );
        }
        assert_eq!(dialer.dials.load(Ordering::SeqCst), 1);
    }

    /// A definitive "not succeeded" is asked again: a succession that lands
    /// later in the session is seen by the next message, not pinned away by
    /// the first.
    #[test]
    fn a_definitive_not_succeeded_is_asked_again() {
        let dialer = CountingDialer::answering(SuccessionLookup::NotSucceeded);
        let memo = SuccessionMemo::new();
        for _ in 0..2 {
            assert_eq!(
                block_on(resolver(&dialer, &memo).resolve_successor(&bound(OLD, ""))),
                None
            );
        }
        assert_eq!(dialer.dials.load(Ordering::SeqCst), 2);
    }

    /// A lookup that made no dial is never remembered, so a config that reads
    /// on the next message is consulted then.
    #[test]
    fn a_lookup_that_made_no_dial_is_asked_again() {
        let dialer = CountingDialer::answering(SuccessionLookup::NotAsked);
        let memo = SuccessionMemo::new();
        for _ in 0..2 {
            assert_eq!(
                block_on(resolver(&dialer, &memo).resolve_successor(&bound(OLD, ""))),
                None
            );
        }
        assert_eq!(dialer.dials.load(Ordering::SeqCst), 2);
    }

    /// The key is `(bound actor, anchor)`: another identity, or the same one
    /// bound at another nest, is its own lookup. A blank binding dials the own
    /// nest and shares its key with an explicit binding to it, whatever the
    /// spelling.
    #[test]
    fn the_memo_is_keyed_on_the_bound_actor_and_its_anchor() {
        let dialer = CountingDialer::answering(SuccessionLookup::Unproven);
        let memo = SuccessionMemo::new();
        let ask = |b: SchedulingPrincipal| {
            block_on(resolver(&dialer, &memo).resolve_successor(&b));
        };
        ask(bound(OLD, ""));
        ask(bound(&OLD.to_ascii_uppercase(), "HTTPS://own.example/"));
        assert_eq!(dialer.dials.load(Ordering::SeqCst), 1);
        ask(bound(OTHER, ""));
        ask(bound(OLD, "https://elsewhere.example"));
        assert_eq!(dialer.dials.load(Ordering::SeqCst), 3);
        assert_eq!(
            *dialer.anchors.lock().unwrap(),
            vec![OWN, OWN, "https://elsewhere.example"]
        );
    }

    /// A fresh memo — a relaunched session — asks again.
    #[test]
    fn a_new_session_asks_again() {
        let dialer = CountingDialer::answering(SuccessionLookup::Unproven);
        block_on(resolver(&dialer, &SuccessionMemo::new()).resolve_successor(&bound(OLD, "")));
        block_on(resolver(&dialer, &SuccessionMemo::new()).resolve_successor(&bound(OLD, "")));
        assert_eq!(dialer.dials.load(Ordering::SeqCst), 2);
    }
}
