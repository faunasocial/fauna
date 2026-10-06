//! What this process's share plane has **served** to peers, per path, plus a
//! hold that parks the next body serve — the e2e observables behind *"an
//! interrupted device-to-device transfer picks up where it stopped without
//! re-sending what arrived"* (`p2p.md` § Cross-user shared-set transfer, "with
//! held chunks never re-sent").
//!
//! **Why the SENDER counts.** "Never re-sent" is a claim about the wire, and
//! the side that knows what crossed it is the one answering the requests. The
//! receiver can say what it materialized, but a receiver that re-fetched a file
//! it already held and then judged it current would read exactly like one that
//! never asked. The serve side ([`crate::peer_share_store::ShareServeSource`])
//! records one manifest and one chunk per successful answer, keyed by the path
//! the answer was for, so a journey can assert *each file was served once*.
//!
//! **Why a hold.** A small file crosses in milliseconds, so dropping the
//! connection "part-way" is a race a journey cannot win on timing (convention
//! 14). The hold makes the middle of a transfer a STATE: while it is on, the
//! next manifest request parks instead of being answered, and
//! [`parked`](ServeTally::parked) says so. The journey drops the connection at
//! that state-defined moment (`offline_share_drop_connections`), then lifts the
//! hold, and the parked request fails rather than answering on a dead channel.
//! It is never counted as served, because nothing reached the peer.
//!
//! Process-wide by design: the plane's serve sources are rebuilt every pump
//! pass, so a per-source counter would reset under the journey's feet, and a
//! process hosts one seat. Paths from different sets are not disambiguated,
//! which is fine for a journey that shares one set.
//!
//! Compiled out of release artifacts (`e2e-automation-surface-gating.md`
//! convention 15), the way [`crate::ceremony_clock`] is. The statics, the
//! setter and the reads sit behind this crate's own `e2e-agent` feature, so in
//! a release build the recorders do nothing, the hold can never be on, and the
//! serve path is exactly what it was.

#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
mod imp {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};

    #[derive(Default)]
    pub(super) struct Counts {
        pub manifests: BTreeMap<String, u64>,
        pub chunks: BTreeMap<String, u64>,
        pub parked: u64,
    }

    pub(super) fn counts() -> &'static Mutex<Counts> {
        static COUNTS: OnceLock<Mutex<Counts>> = OnceLock::new();
        COUNTS.get_or_init(Mutex::default)
    }

    /// `true` while the hold is on.
    pub(super) fn hold() -> &'static tokio::sync::watch::Sender<bool> {
        static HOLD: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();
        HOLD.get_or_init(|| tokio::sync::watch::Sender::new(false))
    }
}

/// A snapshot of the tally — the `share_serve_tally` state key's body.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ServeTally {
    /// path → manifests answered for it.
    pub manifests: std::collections::BTreeMap<String, u64>,
    /// path → chunk bodies answered for it.
    pub chunks: std::collections::BTreeMap<String, u64>,
    /// Requests currently parked on the hold. Above zero, a transfer is
    /// provably in flight and waiting.
    pub parked: u64,
    /// Whether the hold is on.
    pub held: bool,
}

/// Record one manifest answered for `path`.
pub fn record_manifest_served(path: &str) {
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    {
        let mut c = imp::counts().lock().expect("serve tally poisoned");
        *c.manifests.entry(path.to_string()).or_default() += 1;
    }
    #[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
    let _ = path;
}

/// Record one chunk body answered for `path`.
pub fn record_chunk_served(path: &str) {
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    {
        let mut c = imp::counts().lock().expect("serve tally poisoned");
        *c.chunks.entry(path.to_string()).or_default() += 1;
    }
    #[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
    let _ = path;
}

/// Park while the hold is on, then fail. A request that was parked is never
/// answered: the hold exists so a journey can cut the connection under it,
/// and answering after the cut would count bytes that never arrived.
/// Returns `Ok(())` immediately when the hold is off, which is always the case
/// in a release build.
pub async fn wait_if_held() -> anyhow::Result<()> {
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    {
        let mut rx = imp::hold().subscribe();
        if !*rx.borrow_and_update() {
            return Ok(());
        }
        imp::counts().lock().expect("serve tally poisoned").parked += 1;
        let _ = rx.wait_for(|held| !*held).await;
        imp::counts().lock().expect("serve tally poisoned").parked -= 1;
        anyhow::bail!("share serve: the request was parked on the e2e hold and is not answered");
    }
    #[cfg(not(any(test, debug_assertions, feature = "e2e-agent")))]
    Ok(())
}

/// Turn the hold on or off. Lifting it wakes every parked request, and each
/// one fails ([`wait_if_held`]).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn set_hold(on: bool) {
    imp::hold().send_replace(on);
}

/// The tally as it stands. A plain lock-and-clone, so it is legal on a paint or
/// state path (convention 11's corollary).
#[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
pub fn snapshot() -> ServeTally {
    let c = imp::counts().lock().expect("serve tally poisoned");
    ServeTally {
        manifests: c.manifests.clone(),
        chunks: c.chunks.clone(),
        parked: c.parked,
        held: *imp::hold().borrow(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test, because the tally is process-wide: separate tests would race
    // each other's counts under the parallel harness.
    #[tokio::test]
    async fn counts_per_path_and_a_parked_request_fails_unanswered() {
        record_manifest_served("tally-test/a.txt");
        record_chunk_served("tally-test/a.txt");
        record_chunk_served("tally-test/a.txt");
        let snap = snapshot();
        assert_eq!(snap.manifests.get("tally-test/a.txt"), Some(&1));
        assert_eq!(snap.chunks.get("tally-test/a.txt"), Some(&2));

        // Hold off: nothing parks.
        wait_if_held().await.expect("no hold, no park");

        set_hold(true);
        let parked = tokio::spawn(wait_if_held());
        // State-defined, not timed: poll until the request has parked.
        while snapshot().parked == 0 {
            tokio::task::yield_now().await;
        }
        assert!(snapshot().held);
        set_hold(false);
        let outcome = parked.await.expect("join");
        assert!(outcome.is_err(), "a parked request is never answered");
        assert_eq!(snapshot().parked, 0);
        assert!(!snapshot().held);
    }
}
