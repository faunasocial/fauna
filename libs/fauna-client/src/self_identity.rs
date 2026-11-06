//! **The one follower of a session's self-identity watch** — every app's
//! account cache and self address read who the session is signed in as from
//! the session mint's own reply ([`BearerSource::subscribe_self_identity`]),
//! never from a challenge of its own (`transport-connection.md` § The dial
//! budget), and every app reads it through [`SelfIdentityFollower`].
//!
//! A launch machine outlives one session's account (tui's long-lived machine,
//! Android's singleton), so its next account's identity must never land on the
//! old account's row. The follower judges each value it hands out twice —
//! the bearer still mints for the session's actor, AND the value it actually
//! read was minted for that actor ([`MintedIdentity::is_for`]): the machine
//! can move to another account between the two reads, and only the second
//! names the value written. Either check failing ends the follower for good.
//! Apps keep no copy of either check: tui and linux drive it in a loop, the
//! FFI's pull-shaped subscription (apple) wraps it.

use std::sync::Arc;

use fauna_nest_http::{BearerSource, MintedIdentity, SelfIdentityWatch};

/// Pull-shaped follower over one self-identity watch: each
/// [`next`](Self::next) resolves with the current identity at once (if a mint
/// has already landed), then with each change, and `None` once the follower
/// has ended — the watch's sender went, or the bearer source moved to another
/// actor. Build it with [`crate::AuthClient::follow_self_identity`]
/// (or [`crate::NestClient::follow_self_identity`]).
pub struct SelfIdentityFollower {
    identity: SelfIdentityWatch,
    bearer: Arc<dyn BearerSource>,
    actor_id: [u8; 32],
    ended: bool,
}

impl SelfIdentityFollower {
    /// Follow `identity` for the session whose actor is `actor_id`, with
    /// `bearer` the source publishing it. The current value counts as unseen,
    /// so the first [`next`](Self::next) delivers an identity that has
    /// already landed — the session client usually minted before the app
    /// subscribed.
    pub fn new(
        mut identity: SelfIdentityWatch,
        bearer: Arc<dyn BearerSource>,
        actor_id: [u8; 32],
    ) -> Self {
        identity.mark_changed();
        Self {
            identity,
            bearer,
            actor_id,
            ended: false,
        }
    }

    /// The next identity confirmed for this session's actor, or `None` once
    /// the follower has ended (and on every call after). Never yields a "not
    /// yet": it waits until a mint has confirmed one.
    pub async fn next(&mut self) -> Option<MintedIdentity> {
        while !self.ended {
            if self.identity.changed().await.is_err() {
                self.ended = true;
                break;
            }
            let read = self.identity.borrow_and_update().clone();
            match judge(self.bearer.bearer_actor_id(), read, &self.actor_id) {
                Judgement::Apply(minted) => return Some(minted),
                Judgement::Wait => {}
                Judgement::End => self.ended = true,
            }
        }
        None
    }
}

/// What a follower does with one value read off the watch.
#[derive(Debug, PartialEq, Eq)]
enum Judgement {
    /// Minted for this session's actor while its bearer mints for it: apply.
    Apply(MintedIdentity),
    /// No mint has confirmed an identity yet: wait for the next change.
    Wait,
    /// The bearer source moved to another actor, or the value read was minted
    /// for one: nothing it publishes is this session's any more.
    End,
}

/// The per-value judgement — the ONE copy of both checks.
fn judge(
    bearer_actor: Option<[u8; 32]>,
    read: Option<MintedIdentity>,
    actor_id: &[u8; 32],
) -> Judgement {
    if bearer_actor.is_some_and(|minted_for| minted_for != *actor_id) {
        return Judgement::End;
    }
    match read {
        None => Judgement::Wait,
        // Judge the value actually read, not the bearer check above: the
        // machine can move to another account between the two, and the
        // identity names the actor it was minted for.
        Some(minted) if !minted.is_for(actor_id) => Judgement::End,
        Some(minted) => Judgement::Apply(minted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    use fauna_nest_http::ApiError;
    use tokio::sync::watch;

    const ME: [u8; 32] = [1; 32];
    const OTHER: [u8; 32] = [2; 32];

    /// The session mint stand-in: names whichever actor the test sets.
    struct Mint(Mutex<Option<[u8; 32]>>);

    #[async_trait::async_trait]
    impl BearerSource for Mint {
        async fn bearer(&self) -> Result<String, ApiError> {
            Err(ApiError::Transport("this test mints nothing".into()))
        }
        fn bearer_actor_id(&self) -> Option<[u8; 32]> {
            *self.0.lock().unwrap()
        }
    }

    fn minted(actor_id: [u8; 32], handle: &str) -> Option<MintedIdentity> {
        Some(MintedIdentity {
            actor_id,
            handle: handle.into(),
            domain: "nest.example".into(),
            tier: "free".into(),
        })
    }

    fn follower(
        initial: Option<MintedIdentity>,
        bearer_actor: Option<[u8; 32]>,
    ) -> (
        SelfIdentityFollower,
        watch::Sender<Option<MintedIdentity>>,
        Arc<Mint>,
    ) {
        let (tx, rx) = watch::channel(initial);
        let mint = Arc::new(Mint(Mutex::new(bearer_actor)));
        let f = SelfIdentityFollower::new(rx, Arc::clone(&mint) as _, ME);
        (f, tx, mint)
    }

    async fn next_within(f: &mut SelfIdentityFollower) -> Option<Option<MintedIdentity>> {
        tokio::time::timeout(Duration::from_millis(300), f.next())
            .await
            .ok()
    }

    /// The current identity lands at once, a later mint's (a rename) next.
    #[tokio::test]
    async fn the_current_identity_and_each_later_one_are_delivered() {
        let (mut f, tx, _mint) = follower(minted(ME, "alice"), Some(ME));
        assert_eq!(next_within(&mut f).await, Some(minted(ME, "alice")));
        tx.send_replace(minted(ME, "alice2"));
        assert_eq!(next_within(&mut f).await, Some(minted(ME, "alice2")));
    }

    /// No mint yet → no value, and no end either.
    #[tokio::test]
    async fn an_empty_watch_waits_for_the_first_mint() {
        let (mut f, tx, _mint) = follower(None, Some(ME));
        assert_eq!(next_within(&mut f).await, None, "nothing to deliver yet");
        tx.send_replace(minted(ME, "alice"));
        assert_eq!(next_within(&mut f).await, Some(minted(ME, "alice")));
    }

    /// The race the bearer check alone left open: the bearer
    /// still names this session's actor while the watch already holds another
    /// account's identity. The follower judges the value it read, so that
    /// identity is never handed out — and the follower ends.
    #[tokio::test]
    async fn an_identity_minted_for_another_actor_never_lands_even_when_the_bearer_check_passed() {
        let (mut f, tx, _mint) = follower(minted(OTHER, "someone-else"), Some(ME));
        assert_eq!(
            next_within(&mut f).await,
            Some(None),
            "another actor's identity ends the follower instead of landing"
        );
        tx.send_replace(minted(ME, "alice"));
        assert_eq!(
            next_within(&mut f).await,
            Some(None),
            "an ended follower stays ended"
        );
    }

    /// Once the bearer source mints for another actor, nothing it publishes
    /// is this session's — even a value still naming this session's actor.
    #[tokio::test]
    async fn the_follower_ends_when_the_bearer_moves_to_another_actor() {
        let (mut f, tx, mint) = follower(minted(ME, "alice"), Some(ME));
        assert_eq!(next_within(&mut f).await, Some(minted(ME, "alice")));
        *mint.0.lock().unwrap() = Some(OTHER);
        tx.send_replace(minted(ME, "alice-late"));
        assert_eq!(next_within(&mut f).await, Some(None));
    }

    /// The watch's sender gone (the bearer source dropped) ends the follower.
    #[tokio::test]
    async fn the_follower_ends_when_the_watch_closes() {
        let (mut f, tx, _mint) = follower(minted(ME, "alice"), Some(ME));
        assert_eq!(next_within(&mut f).await, Some(minted(ME, "alice")));
        drop(tx);
        assert_eq!(next_within(&mut f).await, Some(None));
    }
}
