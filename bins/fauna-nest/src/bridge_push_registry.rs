//! In-memory registry for `fauna.bridges.subscribe_mailbox_state` push
//! subscriptions. Per `docs/goal/behavior/imap-server.md` § IDLE /
//! NOTIFY: each MDA-mediated IMAP session that has entered IDLE or
//! issued NOTIFY registers interest in a `(served_user_actor_id,
//! mailbox)` pair; nest emits `BridgeMailboxStatePush` frames to the
//! registered MDA whenever a state-mutating handler commits a change
//! against that mailbox.
//!
//! The registry is intentionally **in-memory only** (claim 7): MUAs
//! re-issue their NOTIFY on
//! reconnect, so persistent storage would add complexity without
//! protocol benefit. WS-connection lifetime owns the entry: once the
//! calling MDA's last WS connection drops, the entries are stale but
//! harmless (the next `emit` simply finds no live subscribers to
//! deliver to via `WsState::notify_push`).
//!
//! **Except after revocation.** "Stale but harmless" holds only while
//! the MDA cannot reconnect *silently*: a revoked bridge keeps its
//! `users` row (the mint gate does not consult `bridge_service_users`),
//! so a leaked key can re-mint and re-open a socket that lands right
//! back in `WsState.subs[pk]` — and a Push is not an RPC, so no
//! dispatch gate ever runs. Stale entries would then re-arm the
//! mailbox-state stream with zero dispatches. Revocation therefore
//! purges the MDA's entries via [`BridgePushRegistry::remove_mda`]
//! (`AppState::revoke_actor_authority`); re-subscribing needs a
//! `subscribe_mailbox_state` dispatch, which `caller_class_for_actor`
//! denies a non-Approved bridge. Per `transport.md` § Connection
//! lifecycle → *Revocation teardown*.
//!
//! Subscription routing model:
//! - The *served user's* `actor_id` is the lookup key (claim 1).
//! - The *MDA service-user's* `actor_id` is stored per subscription
//!   row so the emitter can reach the right WS connection: nest pushes
//!   travel along the WS-RPC connection identified by `actor_id =
//!   MDA-service-user`, with the served user's identity inside the
//!   payload.
//! - The MDA's notification router demuxes incoming pushes by
//!   `subscription_id`, so multiple IMAP sessions on the same MDA can
//!   subscribe to overlapping `(user, mailbox)` pairs without
//!   ambiguity.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use fauna_protocol::PushEvent;
use fauna_protocol::bridge_routing::{BridgeMailboxStatePush, MailboxStateEvent};

use crate::ws::WsState;

/// One subscription row: who registered (`mda_actor_id`) and the
/// demux id we hand back to them.
#[derive(Debug, Clone, Copy)]
pub struct SubscriptionEntry {
    pub subscription_id: u64,
    pub mda_actor_id: [u8; 32],
}

/// In-memory subscription map: `(served_user_actor_id, mailbox) →
/// list of MDA subscribers`. The `Vec` is cheap (NOTIFY's per-session
/// registration count is tiny — handful per active MUA).
pub struct BridgePushRegistry {
    next_subscription_id: AtomicU64,
    subs: Mutex<HashMap<(Vec<u8>, String), Vec<SubscriptionEntry>>>,
}

impl Default for BridgePushRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BridgePushRegistry {
    pub fn new() -> Self {
        Self {
            // Start at 1 so callers can use `0` as a sentinel for
            // "no subscription".
            next_subscription_id: AtomicU64::new(1),
            subs: Mutex::new(HashMap::new()),
        }
    }

    /// Record a new subscription. Returns the allocated
    /// `subscription_id` (monotonic per registry instance).
    pub fn register(
        &self,
        served_user_actor_id: &[u8],
        mailbox: &str,
        mda_actor_id: [u8; 32],
    ) -> u64 {
        let id = self.next_subscription_id.fetch_add(1, Ordering::Relaxed);
        let entry = SubscriptionEntry {
            subscription_id: id,
            mda_actor_id,
        };
        let key = (served_user_actor_id.to_vec(), mailbox.to_string());
        self.subs
            .lock()
            .unwrap()
            .entry(key)
            .or_default()
            .push(entry);
        id
    }

    /// Snapshot all subscriptions matching `(served_user, mailbox)`.
    /// Returns the per-MDA entries; callers iterate and emit per
    /// subscription so each MDA's notification router gets a frame
    /// it can demux by `subscription_id`.
    pub fn matching(&self, served_user_actor_id: &[u8], mailbox: &str) -> Vec<SubscriptionEntry> {
        let key = (served_user_actor_id.to_vec(), mailbox.to_string());
        self.subs
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_default()
    }

    /// Emit `event` to every subscription matching `(served_user_actor_id,
    /// mailbox)`. One `Frame::Push` per subscription is queued onto the
    /// receiving MDA's WS connection(s); each push carries its unique
    /// `subscription_id` so the MDA can route to the right IMAP
    /// session. Best-effort: a subscription whose MDA actor has no
    /// live WS connection is silently skipped (the connection-reaper
    /// will eventually drop the entry; F.2 wires the explicit
    /// drop-on-close path).
    ///
    /// The hook fires *after* the SQLite transaction commits — see
    /// each emission site in `bridge_imap_handlers.rs` (Spec § D6
    /// (ε) atomicity rule).
    pub fn emit(
        &self,
        ws: &WsState,
        served_user_actor_id: &[u8],
        mailbox: &str,
        event: &MailboxStateEvent,
    ) {
        let entries = self.matching(served_user_actor_id, mailbox);
        for entry in entries {
            let push = BridgeMailboxStatePush {
                subscription_id: entry.subscription_id,
                actor_id: served_user_actor_id.to_vec(),
                mailbox: mailbox.to_string(),
                event: event.clone(),
            };
            ws.notify_push(&entry.mda_actor_id, PushEvent::BridgeMailboxState(push));
        }
    }

    /// Drop every subscription registered by `mda_actor_id`, across all
    /// `(served_user, mailbox)` keys; keys left empty are removed. Returns
    /// how many subscription rows were dropped.
    ///
    /// The revocation-teardown hook (module doc): called when the MDA's
    /// bridge service user is revoked, so a socket the revoked bridge
    /// re-opens with a still-valid `users` row receives nothing — the
    /// entries are gone and re-registering needs a dispatch the capability
    /// gate denies. Idempotent; a no-op for an actor with no entries
    /// (every non-MDA caller of `AppState::revoke_actor_authority`).
    pub fn remove_mda(&self, mda_actor_id: &[u8; 32]) -> usize {
        let mut subs = self.subs.lock().unwrap();
        let mut dropped = 0;
        subs.retain(|_, entries| {
            let before = entries.len();
            entries.retain(|e| &e.mda_actor_id != mda_actor_id);
            dropped += before - entries.len();
            !entries.is_empty()
        });
        dropped
    }

    /// Returns the number of distinct `(served_user, mailbox)` keys
    /// currently registered. Exposed for tests + observability.
    pub fn key_count(&self) -> usize {
        self.subs.lock().unwrap().len()
    }

    /// Returns the total number of subscription rows across all keys.
    /// Exposed for tests + observability.
    pub fn subscription_count(&self) -> usize {
        self.subs.lock().unwrap().values().map(|v| v.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::Frame;
    use fauna_protocol::decode_frame;

    #[test]
    fn registry_allocates_unique_ids() {
        let r = BridgePushRegistry::new();
        let id1 = r.register(&[1u8; 32], "INBOX", [9u8; 32]);
        let id2 = r.register(&[1u8; 32], "INBOX", [9u8; 32]);
        let id3 = r.register(&[1u8; 32], "Sent", [9u8; 32]);
        assert_ne!(id1, id2);
        assert_ne!(id1, id3);
        assert_ne!(id2, id3);
        assert!(id1 >= 1, "ids start at 1");
    }

    #[test]
    fn matching_returns_only_matching_key() {
        let r = BridgePushRegistry::new();
        r.register(&[1u8; 32], "INBOX", [9u8; 32]);
        r.register(&[2u8; 32], "INBOX", [9u8; 32]);
        r.register(&[1u8; 32], "Sent", [9u8; 32]);

        let m = r.matching(&[1u8; 32], "INBOX");
        assert_eq!(m.len(), 1, "exactly one subscription matches");
        let m = r.matching(&[2u8; 32], "INBOX");
        assert_eq!(m.len(), 1);
        let m = r.matching(&[1u8; 32], "Sent");
        assert_eq!(m.len(), 1);
        let m = r.matching(&[3u8; 32], "INBOX");
        assert!(m.is_empty(), "non-existent actor returns empty");
    }

    #[test]
    fn emit_pushes_once_per_subscription_with_unique_ids() {
        let ws = WsState::new();
        let r = BridgePushRegistry::new();
        let mda = [7u8; 32];
        let (_conn, mut rx) = ws.subscribe(mda);

        let id1 = r.register(&[1u8; 32], "INBOX", mda);
        let id2 = r.register(&[1u8; 32], "INBOX", mda);

        r.emit(
            &ws,
            &[1u8; 32],
            "INBOX",
            &MailboxStateEvent::Append {
                uid: 42,
                flags: vec!["\\Recent".into()],
                modseq: 100,
            },
        );

        // Two pushes on the same WS connection — one per subscription.
        let mut seen_ids = Vec::new();
        for _ in 0..2 {
            let bytes = rx.try_recv().expect("expected push");
            let frame = decode_frame(&bytes).unwrap();
            match frame {
                Frame::Push(p) => {
                    assert_eq!(p.kind, "fauna.bridges.push.mailbox_state");
                    let payload_bytes = fauna_protocol::encode_canonical(&p.payload).unwrap();
                    let push: BridgeMailboxStatePush =
                        fauna_cbor::decode_strict(&payload_bytes).unwrap();
                    seen_ids.push(push.subscription_id);
                    assert_eq!(push.mailbox, "INBOX");
                    assert_eq!(push.actor_id, vec![1u8; 32]);
                    match push.event {
                        MailboxStateEvent::Append { uid, modseq, .. } => {
                            assert_eq!(uid, 42);
                            assert_eq!(modseq, 100);
                        }
                        _ => panic!("expected Append"),
                    }
                }
                _ => panic!("expected Push"),
            }
        }
        seen_ids.sort();
        let mut expect = vec![id1, id2];
        expect.sort();
        assert_eq!(seen_ids, expect);
    }

    #[test]
    fn emit_with_no_match_is_silent_noop() {
        let ws = WsState::new();
        let r = BridgePushRegistry::new();
        let mda = [7u8; 32];
        let (_conn, mut rx) = ws.subscribe(mda);

        // Subscribe to INBOX; emit on Sent.
        r.register(&[1u8; 32], "INBOX", mda);
        r.emit(
            &ws,
            &[1u8; 32],
            "Sent",
            &MailboxStateEvent::Expunge { uid: 1, modseq: 10 },
        );

        assert!(rx.try_recv().is_err(), "no push expected");
    }

    #[test]
    fn emit_targets_correct_mda_when_two_subscribe() {
        let ws = WsState::new();
        let r = BridgePushRegistry::new();
        let mda_a = [7u8; 32];
        let mda_b = [8u8; 32];
        let (_conn_a, mut rx_a) = ws.subscribe(mda_a);
        let (_conn_b, mut rx_b) = ws.subscribe(mda_b);

        // Two MDAs subscribed to the same (user, mailbox).
        r.register(&[1u8; 32], "INBOX", mda_a);
        r.register(&[1u8; 32], "INBOX", mda_b);

        r.emit(
            &ws,
            &[1u8; 32],
            "INBOX",
            &MailboxStateEvent::Flags {
                uid: 3,
                flags: vec!["\\Seen".into()],
                modseq: 5,
            },
        );

        // Each MDA receives exactly one push.
        let _ = rx_a.try_recv().expect("MDA a received");
        assert!(rx_a.try_recv().is_err(), "MDA a received exactly one");
        let _ = rx_b.try_recv().expect("MDA b received");
        assert!(rx_b.try_recv().is_err(), "MDA b received exactly one");
    }

    #[test]
    fn counts_track_register_calls() {
        let r = BridgePushRegistry::new();
        assert_eq!(r.key_count(), 0);
        assert_eq!(r.subscription_count(), 0);

        r.register(&[1u8; 32], "INBOX", [9u8; 32]);
        assert_eq!(r.key_count(), 1);
        assert_eq!(r.subscription_count(), 1);

        // Same key, second subscription — key_count unchanged.
        r.register(&[1u8; 32], "INBOX", [9u8; 32]);
        assert_eq!(r.key_count(), 1);
        assert_eq!(r.subscription_count(), 2);

        // Different mailbox — new key.
        r.register(&[1u8; 32], "Sent", [9u8; 32]);
        assert_eq!(r.key_count(), 2);
        assert_eq!(r.subscription_count(), 3);
    }

    #[test]
    fn remove_mda_drops_only_that_mdas_entries_across_all_keys() {
        let r = BridgePushRegistry::new();
        let revoked = [7u8; 32];
        let survivor = [8u8; 32];
        // The revoked MDA holds entries under two keys, sharing one key
        // with the surviving MDA.
        r.register(&[1u8; 32], "INBOX", revoked);
        r.register(&[1u8; 32], "INBOX", survivor);
        r.register(&[2u8; 32], "Sent", revoked);

        assert_eq!(r.remove_mda(&revoked), 2);

        // The survivor's entry is untouched; the key the revoked MDA held
        // alone is gone entirely.
        assert_eq!(r.matching(&[1u8; 32], "INBOX").len(), 1);
        assert_eq!(r.matching(&[1u8; 32], "INBOX")[0].mda_actor_id, survivor);
        assert!(r.matching(&[2u8; 32], "Sent").is_empty());
        assert_eq!(r.key_count(), 1);
        assert_eq!(r.subscription_count(), 1);
    }

    #[test]
    fn remove_mda_is_idempotent_and_a_noop_for_unknown_actor() {
        let r = BridgePushRegistry::new();
        r.register(&[1u8; 32], "INBOX", [7u8; 32]);

        assert_eq!(r.remove_mda(&[9u8; 32]), 0, "unknown actor: no-op");
        assert_eq!(r.remove_mda(&[7u8; 32]), 1);
        assert_eq!(r.remove_mda(&[7u8; 32]), 0, "second call: idempotent");
        assert_eq!(r.subscription_count(), 0);
    }

    #[test]
    fn emit_after_remove_mda_delivers_nothing_to_a_live_socket() {
        // The revocation scenario end-to-end at this layer: the revoked
        // MDA still holds a live WS connection (or re-opened one), but its
        // registry entries are purged — emit must deliver nothing.
        let ws = WsState::new();
        let r = BridgePushRegistry::new();
        let mda = [7u8; 32];
        let (_conn, mut rx) = ws.subscribe(mda);
        r.register(&[1u8; 32], "INBOX", mda);

        r.remove_mda(&mda);
        r.emit(
            &ws,
            &[1u8; 32],
            "INBOX",
            &MailboxStateEvent::Append {
                uid: 1,
                flags: vec![],
                modseq: 1,
            },
        );

        assert!(rx.try_recv().is_err(), "purged MDA must receive nothing");
    }
}
