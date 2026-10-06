#![cfg(feature = "nostr")]
//! Focused integration test for the persistent NIP-01 relay event store
//! (`docs/goal/ui/nostr.md` § The relay event store, slices A/B/C).
//!
//! Proves the success criterion's core property — **restart survival** — that
//! the store's in-process unit tests (`nostr::store`) cannot: an event written
//! by a local account survives a process restart (a fresh SQLite connection to
//! the same on-disk file) and comes back from a query, with correct
//! replaceable / ephemeral / duplicate semantics preserved across the restart,
//! and with a still-valid signature (the relay never emits an unsigned event).
//!
//! Slice C adds deletion (NIP-09), expiration (NIP-40), and COUNT (NIP-45) to
//! that same restart-survival bar: a delete or an expiration recorded before
//! a restart must still hold after it, and COUNT must reflect the
//! post-restart state.

use fauna_bridge_nostr::signing::{Keypair, verify_event};
use fauna_bridge_nostr::types::{Event, Filter, Tag, UnsignedEvent};
use fauna_nest::nostr::store::{self, StoreOutcome};
use rusqlite::Connection;

/// Open (or reopen) the nest DB file with the base + nostr schema — the second
/// call in a test models a nest restart against the same on-disk state.
fn open_db(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    fauna_nest::db::migrations::run_migrations(&conn).unwrap();
    fauna_nest::nostr::apply_schema(&conn).unwrap();
    conn
}

fn signed(kp: &Keypair, kind: u64, created_at: u64, tags: Vec<Tag>, content: &str) -> Event {
    kp.sign_event(UnsignedEvent {
        pubkey: kp.public_key_bytes(),
        created_at,
        kind,
        tags,
        content: content.to_string(),
    })
}

#[test]
fn event_survives_restart_with_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nest.db");
    let kp = Keypair::from_secret_bytes([1u8; 32]).unwrap();

    // ── Session 1: a local account publishes events, then the nest "stops". ──
    {
        let conn = open_db(&path);

        let note = signed(&kp, 1, 1000, vec![], "hello from before restart");
        assert_eq!(
            store::store_event(&conn, &note, false).unwrap(),
            StoreOutcome::Stored
        );

        // A replaceable profile (kind 0), version 1.
        let profile_v1 = signed(&kp, 0, 1000, vec![], "profile v1");
        assert_eq!(
            store::store_event(&conn, &profile_v1, false).unwrap(),
            StoreOutcome::Stored
        );

        // An ephemeral kind must never persist.
        let ephemeral = signed(&kp, 20_000, 1000, vec![], "ephemeral");
        assert_eq!(
            store::store_event(&conn, &ephemeral, false).unwrap(),
            StoreOutcome::Ephemeral
        );
        // conn dropped here → connection closed, DB flushed to disk.
    }

    // ── Session 2: the nest restarts (fresh connection to the same file). ──
    {
        let conn = open_db(&path);

        // Duplicate detection survives the restart.
        let note_again = signed(&kp, 1, 1000, vec![], "hello from before restart");
        assert_eq!(
            store::store_event(&conn, &note_again, false).unwrap(),
            StoreOutcome::Duplicate
        );

        // Replaceable semantics survive: a newer profile replaces the persisted
        // v1 (keyed on the pre-restart row).
        let profile_v2 = signed(&kp, 0, 2000, vec![], "profile v2");
        assert_eq!(
            store::store_event(&conn, &profile_v2, false).unwrap(),
            StoreOutcome::Replaced
        );

        // A REQ over the store returns the surviving events, correctly.
        let all = store::query_events(&conn, &[Filter::default()], 100).unwrap();
        assert_eq!(all.len(), 2, "note + profile v2 survive; ephemeral gone");

        let note = all
            .iter()
            .find(|e| e.content == "hello from before restart")
            .expect("the pre-restart note survived");
        assert!(
            verify_event(note),
            "surviving event carries a valid signature"
        );

        assert!(
            all.iter().any(|e| e.content == "profile v2"),
            "the newer replaceable profile is served"
        );
        assert!(
            !all.iter().any(|e| e.content == "profile v1"),
            "the superseded profile is gone"
        );
        assert!(
            !all.iter().any(|e| e.content == "ephemeral"),
            "ephemeral events are never persisted"
        );
    }
}

#[test]
fn author_and_kind_filters_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nest.db");
    let alice = Keypair::from_secret_bytes([1u8; 32]).unwrap();
    let bob = Keypair::from_secret_bytes([2u8; 32]).unwrap();

    {
        let conn = open_db(&path);
        store::store_event(&conn, &signed(&alice, 1, 1000, vec![], "alice note"), false).unwrap();
        store::store_event(&conn, &signed(&bob, 1, 2000, vec![], "bob note"), false).unwrap();
        let target = "d".repeat(64);
        store::store_event(
            &conn,
            &signed(
                &alice,
                7,
                3000,
                vec![Tag::new(vec!["e".into(), target])],
                "alice reaction",
            ),
            false,
        )
        .unwrap();
    }

    {
        let conn = open_db(&path);

        // author filter
        let f = Filter {
            authors: Some(vec![alice.public_key_hex()]),
            ..Default::default()
        };
        let got = store::query_events(&conn, &[f], 100).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|e| e.pubkey == alice.public_key_hex()));

        // kind + tag filter, newest-first ordering
        let mut tags = std::collections::HashMap::new();
        tags.insert("#e".to_string(), vec!["d".repeat(64)]);
        let f = Filter {
            kinds: Some(vec![7]),
            tags,
            ..Default::default()
        };
        let got = store::query_events(&conn, &[f], 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].content, "alice reaction");
    }
}

#[test]
fn deletion_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nest.db");
    let kp = Keypair::from_secret_bytes([3u8; 32]).unwrap();
    let note = signed(&kp, 1, 1000, vec![], "delete me across a restart");

    // ── Session 1: publish, then delete. ──
    {
        let conn = open_db(&path);
        store::store_event(&conn, &note, false).unwrap();
        let deletion = signed(
            &kp,
            5,
            2000,
            vec![Tag::new(vec!["e".into(), note.id.clone()])],
            "",
        );
        store::store_event(&conn, &deletion, false).unwrap();
        assert_eq!(store::apply_deletion(&conn, &deletion).unwrap(), 1);
    }

    // ── Session 2: the nest restarts — the deletion holds. ──
    {
        let conn = open_db(&path);
        let all = store::query_events(&conn, &[Filter::default()], 100).unwrap();
        assert!(
            all.iter().all(|e| e.id != note.id),
            "the deleted event does not reappear after a restart"
        );
        assert!(
            all.iter().any(|e| e.kind == 5),
            "the deletion request itself is a normal stored event"
        );
    }
}

#[test]
fn expiration_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nest.db");
    let kp = Keypair::from_secret_bytes([4u8; 32]).unwrap();

    // A far-future expiration written pre-restart must still gate correctly
    // post-restart; an already-past one must still be rejected outright.
    let now = fauna_core::data::Timestamp::now_secs();
    let future = signed(
        &kp,
        1,
        1000,
        vec![Tag::new(vec![
            "expiration".into(),
            (now + 100_000).to_string(),
        ])],
        "expires far in the future",
    );
    let already_expired = signed(
        &kp,
        1,
        1000,
        vec![Tag::new(vec!["expiration".into(), (now - 100).to_string()])],
        "already expired",
    );

    {
        let conn = open_db(&path);
        assert_eq!(
            store::store_event(&conn, &future, false).unwrap(),
            StoreOutcome::Stored
        );
        assert_eq!(
            store::store_event(&conn, &already_expired, false).unwrap(),
            StoreOutcome::Expired
        );
    }

    {
        let conn = open_db(&path);
        let all = store::query_events(&conn, &[Filter::default()], 100).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].content, "expires far in the future");
    }
}

#[test]
fn count_reflects_state_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nest.db");
    let kp = Keypair::from_secret_bytes([5u8; 32]).unwrap();

    {
        let conn = open_db(&path);
        store::store_event(&conn, &signed(&kp, 1, 1000, vec![], "a"), false).unwrap();
        store::store_event(&conn, &signed(&kp, 1, 2000, vec![], "b"), false).unwrap();
        store::store_event(&conn, &signed(&kp, 7, 3000, vec![], "reaction"), false).unwrap();
    }

    {
        let conn = open_db(&path);
        let f = Filter {
            kinds: Some(vec![1]),
            ..Default::default()
        };
        let result = store::count_events(&conn, &[f], None).unwrap();
        assert_eq!(result.count, 2);
        assert!(!result.approximate);
    }
}

#[test]
fn nip50_search_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nest.db");
    let kp = Keypair::from_secret_bytes([6u8; 32]).unwrap();

    // ── Session 1: events land through the triggers, then the nest stops. ──
    {
        let conn = open_db(&path);
        store::store_event(
            &conn,
            &signed(&kp, 1, 1000, vec![], "company picnic on saturday"),
            false,
        )
        .unwrap();
        store::store_event(
            &conn,
            &signed(&kp, 1, 2000, vec![], "unrelated note"),
            false,
        )
        .unwrap();
    }

    // ── Session 2: the search verdict holds on a fresh connection. ──
    {
        let conn = open_db(&path);
        let f = Filter {
            search: Some("PICNIC".to_string()),
            ..Default::default()
        };
        let got = store::query_events(&conn, std::slice::from_ref(&f), 100).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].content.contains("picnic"));

        // COUNT shares the verdict.
        let count = store::count_events(&conn, &[f], None).unwrap();
        assert_eq!(count.count, 1);
    }
}
