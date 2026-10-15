//! The recipient picker's completion — `docs/goal/ui/conversations.md`
//! § Where logic lives → *Recipient suggestions*: sources (correspondents and
//! the contact roster), matching, exclusions, ranking, the cap, and the click
//! (`accept_suggestion`) being the Enter path rather than a shortcut past it.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use fauna_conversations::backend::{ContactRosterSource, ResolveResult};
use fauna_conversations::backends::mock::MockRailBackend;
use fauna_conversations::compose::ResolveState;
use fauna_conversations::*;
use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};
use fauna_core::identity::ActorId;

fn email(a: &str) -> TypedAddress {
    TypedAddress::Email {
        email_address: a.into(),
    }
}

fn fauna(handle: &str, byte: u8) -> TypedAddress {
    TypedAddress::Fauna {
        handle: handle.into(),
        actor_id: ActorId([byte; 32]),
    }
}

fn roster_entry(byte: u8, handle: &str, status: &str) -> RosterEntry {
    RosterEntry {
        actor_id_hex: hex::encode([byte; 32]),
        handle: Some(handle.into()),
        domain: Some("nest.test".into()),
        status: status.into(),
    }
}

fn manager() -> Arc<ConversationsManager> {
    let m = ConversationsManager::new();
    m.install_mock_backends_for_test();
    m
}

/// The new-thread picker as the snapshot shows it.
fn picker(m: &ConversationsManager) -> RecipientPickerState {
    m.snapshot()
        .new_thread_compose
        .expect("composer open")
        .recipient_picker
        .expect("picker present")
}

fn typed(m: &ConversationsManager, text: &str) -> RecipientPickerState {
    m.set_new_thread_recipient_input(text.into());
    picker(m)
}

#[test]
fn a_prefix_of_a_correspondents_mail_address_suggests_it() {
    let m = manager();
    m.create_mls_group(vec![email("carol@mail.test")]);
    m.start_new_conversation();

    let p = typed(&m, "car");
    assert_eq!(p.suggestions, vec![email("carol@mail.test")]);
    assert_eq!(p.suggestion_displays, vec!["carol@mail.test".to_string()]);

    // Half a domain completes as well.
    assert_eq!(typed(&m, "mail.te").suggestions.len(), 1);
    assert!(typed(&m, "zed").suggestions.is_empty());
}

#[test]
fn empty_input_suggests_nothing() {
    let m = manager();
    m.create_mls_group(vec![email("carol@mail.test")]);
    m.apply_contact_roster(vec![roster_entry(0xb0, "bob", "accepted")]);
    m.start_new_conversation();

    let p = typed(&m, "");
    assert!(p.suggestions.is_empty());
    assert!(p.suggestion_displays.is_empty());
    assert!(typed(&m, "   ").suggestions.is_empty());
}

#[test]
fn a_contacts_nickname_suggests_the_contact_and_contacts_rank_first() {
    let m = manager();
    let bob_hex = hex::encode([0xb0; 32]);
    // Bob is also a correspondent (same actor id): one row, the contact's.
    m.create_mls_group(vec![email("bobby@mail.test"), fauna("", 0xb0)]);
    m.apply_contact_roster(vec![roster_entry(0xb0, "bob", "accepted")]);
    m.contacts().replace(BTreeMap::from([(
        bob_hex.clone(),
        ContactOverlay {
            nickname: Register {
                stamp: Stamp::new(1, [1; 32]),
                value: Some("Uncle B".into()),
            },
            ..Default::default()
        },
    )]));
    m.start_new_conversation();

    let p = typed(&m, "uncle");
    assert_eq!(p.suggestions.len(), 1, "{:?}", p.suggestions);
    assert_eq!(
        p.suggestions[0].person_actor_id(),
        Some(ActorId([0xb0; 32]))
    );
    assert_eq!(p.suggestion_displays, vec!["Uncle B".to_string()]);

    // "bob" matches the contact and the mail correspondent: contact first, and
    // the Fauna correspondent row for the same person is folded into it.
    let p = typed(&m, "bob");
    assert_eq!(p.suggestions.len(), 2, "{:?}", p.suggestions);
    assert_eq!(
        p.suggestions[0].person_actor_id(),
        Some(ActorId([0xb0; 32]))
    );
    assert_eq!(p.suggestions[1], email("bobby@mail.test"));
}

#[test]
fn confirmed_contacts_rank_before_accepted_and_others_are_not_offered() {
    let m = manager();
    m.apply_contact_roster(vec![
        roster_entry(0xa1, "sam-accepted", "accepted"),
        roster_entry(0xa2, "sam-confirmed", "confirmed"),
        roster_entry(0xa3, "sam-blocked", "blocked"),
        roster_entry(0xa4, "sam-pending", "pending"),
    ]);
    m.start_new_conversation();

    let p = typed(&m, "sam");
    let ids: Vec<_> = p.suggestions.iter().map(|a| a.person_actor_id()).collect();
    assert_eq!(
        ids,
        vec![Some(ActorId([0xa2; 32])), Some(ActorId([0xa1; 32]))]
    );
}

#[test]
fn self_held_chips_target_members_and_blocked_people_are_excluded() {
    let m = ConversationsManager::new();
    let mail = Arc::new(MockRailBackend::new(Rail::Smtp));
    mail.set_self_address(email("me@mail.test"));
    m.register_backend(mail);
    m.register_backend(Arc::new(MockRailBackend::new(Rail::FaunaMls)));

    let target = m.create_mls_group(vec![email("ann@mail.test"), email("me@mail.test")]);
    m.create_mls_group(vec![email("amy@mail.test"), email("abe@mail.test")]);
    // A blocked person who is also a correspondent stays hidden.
    m.create_mls_group(vec![fauna("axel", 0xc0)]);
    m.apply_contact_roster(vec![roster_entry(0xc0, "axel", "blocked")]);

    // The add-participant overlay: its thread's members are not offered.
    m.open_add_participant(target);
    m.set_add_participant_recipient_input("a".into());
    let p = m.snapshot().add_participant.unwrap().picker;
    assert!(!p.suggestions.contains(&email("ann@mail.test")));
    assert!(!p.suggestions.contains(&email("me@mail.test")), "self");
    assert!(p.suggestions.contains(&email("amy@mail.test")));
    assert!(
        p.suggestions
            .iter()
            .all(|a| a.person_actor_id() != Some(ActorId([0xc0; 32]))),
        "blocked"
    );
    m.cancel_add_participant();

    // The new-thread picker: a held chip is not offered again.
    m.start_new_conversation();
    m.set_new_thread_recipient_input("amy@mail.test".into());
    let mut p = picker(&m);
    assert!(p.suggestions.contains(&email("amy@mail.test")));
    m.accept_new_thread_chip(email("amy@mail.test"));
    p = typed(&m, "a");
    assert!(!p.suggestions.contains(&email("amy@mail.test")));
    assert!(p.suggestions.contains(&email("ann@mail.test")));
    assert!(!p.suggestions.contains(&email("me@mail.test")), "self");
}

#[test]
fn the_list_is_capped() {
    let m = manager();
    let people: Vec<_> = (0..12)
        .map(|i| email(&format!("pat{i}@mail.test")))
        .collect();
    m.create_mls_group(people);
    m.start_new_conversation();

    let p = typed(&m, "pat");
    assert_eq!(p.suggestions.len(), RECIPIENT_SUGGESTION_CAP);
    assert_eq!(p.suggestion_displays.len(), RECIPIENT_SUGGESTION_CAP);
}

#[tokio::test]
async fn accepting_a_suggestion_runs_the_probe_and_commits_its_answer() {
    let m = manager();
    m.create_mls_group(vec![email("carol@mail.test")]);
    m.start_new_conversation();
    typed(&m, "car");

    assert!(m.accept_suggestion(0).await);
    let p = picker(&m);
    assert_eq!(p.chips, vec![email("carol@mail.test")]);
    assert!(p.raw_input.is_empty());
    assert!(
        p.suggestions.is_empty(),
        "the input cleared with the commit"
    );

    // Past the end of the list: nothing.
    typed(&m, "car");
    assert!(!m.accept_suggestion(5).await);
}

#[tokio::test]
async fn a_suggestion_whose_probe_finds_nobody_commits_nothing() {
    let m = ConversationsManager::new();
    let rail = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    let hex = hex::encode([0xb0; 32]);
    rail.override_resolve(&hex, ResolveResult::NotFound);
    rail.override_resolve("bob@nest.test", ResolveResult::NotFound);
    m.register_backend(rail);
    m.apply_contact_roster(vec![RosterEntry {
        // No handle: the spelling written is the actor id.
        handle: None,
        domain: None,
        ..roster_entry(0xb0, "", "accepted")
    }]);
    m.start_new_conversation();
    assert_eq!(typed(&m, "b0b0").suggestions.len(), 1);

    assert!(
        !m.accept_suggestion(0).await,
        "an unprobed chip never commits"
    );
    let p = picker(&m);
    assert!(p.chips.is_empty());
    assert_eq!(p.raw_input, hex, "the spelling the probe ran on");
    assert_eq!(p.resolve_state, ResolveState::NotFound);
}

#[tokio::test]
async fn a_contacts_suggestion_resolves_through_its_handle() {
    let m = ConversationsManager::new();
    let rail = Arc::new(MockRailBackend::new(Rail::FaunaMls));
    let bob = fauna("bob@nest.test", 0xb0);
    rail.override_resolve("bob@nest.test", ResolveResult::Resolved(bob.clone()));
    m.register_backend(rail);
    m.apply_contact_roster(vec![roster_entry(0xb0, "bob", "confirmed")]);
    m.start_new_conversation();
    let p = typed(&m, "bo");
    assert_eq!(p.suggestions, vec![bob.clone()]);
    assert_eq!(p.suggestion_displays, vec!["bob@nest.test".to_string()]);

    assert!(m.accept_suggestion(0).await);
    assert_eq!(picker(&m).chips, vec![bob]);
}

#[test]
fn remove_recipient_chip_drops_the_active_pickers_chip() {
    let m = manager();
    let target = m.create_mls_group(vec![email("ann@mail.test")]);
    m.start_new_conversation();
    m.accept_new_thread_chip(email("a@mail.test"));
    m.accept_new_thread_chip(email("b@mail.test"));
    m.remove_recipient_chip(0);
    assert_eq!(picker(&m).chips, vec![email("b@mail.test")]);
    m.remove_recipient_chip(7); // past the end: no-op
    assert_eq!(picker(&m).chips.len(), 1);

    // The overlay wins while open.
    m.open_add_participant(target);
    m.accept_add_participant_chip(email("c@mail.test"));
    m.remove_recipient_chip(0);
    assert!(
        m.snapshot()
            .add_participant
            .unwrap()
            .picker
            .chips
            .is_empty()
    );
    assert_eq!(picker(&m).chips.len(), 1, "the new-thread chip untouched");
}

struct FixedRoster(Vec<RosterEntry>);

#[async_trait]
impl ContactRosterSource for FixedRoster {
    async fn roster(&self) -> Result<Vec<RosterEntry>, String> {
        Ok(self.0.clone())
    }
}

struct BrokenRoster;

#[async_trait]
impl ContactRosterSource for BrokenRoster {
    async fn roster(&self) -> Result<Vec<RosterEntry>, String> {
        Err("nest down".into())
    }
}

#[tokio::test]
async fn the_roster_loads_from_its_source_and_a_failed_read_keeps_the_last() {
    let m = manager();
    m.start_new_conversation();
    assert!(typed(&m, "bob").suggestions.is_empty());

    m.register_contact_roster(Arc::new(FixedRoster(vec![roster_entry(
        0xb0, "bob", "accepted",
    )])));
    m.refresh_contact_roster().await;
    assert_eq!(picker(&m).suggestions.len(), 1);

    m.register_contact_roster(Arc::new(BrokenRoster));
    m.refresh_contact_roster().await;
    assert_eq!(picker(&m).suggestions.len(), 1, "the last roster stands");
}
