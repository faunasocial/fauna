//! **Every account holds the key** — the MSEK minted at the account's first
//! connect, whatever the deployment serves.
//! Authority: `docs/goal/behavior/mail-credentials.md` § MSEK lifecycle →
//! *Every account holds the key* (rulings 1–4 and 6).

use fauna_client_mail_settings::testing::{FakeMailStore, FakeNestClient};
use fauna_client_mail_settings::{CredentialKind, MailSettingsAction, NestError, SecretBytes};
use fauna_core::data::MsekFingerprint;
use fauna_core::mail_rows::MailRows;
use fauna_mls::wrapped_blob::DavKind;
use fauna_mls::wrapped_blob::dav_body::{DavRecipientKeys, seal_dav_body};

mod common;
use common::{build, machine_over};

/// Nothing reached the nest: no snapshot, registration, wrapped blob,
/// submission token, revoke or flag (ruling 1).
fn assert_nest_untouched(nest: &FakeNestClient) {
    let s = nest.state();
    assert!(s.provision_mls_snapshot.is_empty(), "no snapshot");
    assert!(s.provision_recipient_pubkey.is_empty(), "no registration");
    assert!(s.provision_wrapped_mls.is_empty(), "no wrapped blob");
    assert!(
        s.provision_submission_token.is_empty(),
        "no submission token"
    );
    assert!(s.revoke_wrapped_mls.is_empty() && s.revoke_submission_token.is_empty());
    assert!(s.set_mail_enabled.is_empty() && s.set_actor_mail_enabled.is_empty());
}

/// Make the one nest read the refresh could take fail, so a stray call
/// surfaces as an error rather than passing silently.
fn fail_nest_reads(nest: &FakeNestClient) {
    nest.state().fail_fetch_recipient_pubkey_with =
        Some(NestError::Transient("the test nest answers nothing".into()));
}

/// **A fresh account mints its key at its first connect** (rulings 1 and 3):
/// the connect-time step leaves the custody holding a key, born in the ring
/// (its own generation row), no credential row, no email flag — and touches
/// no nest. A second connect mints nothing more.
#[tokio::test]
async fn a_fresh_account_mints_its_key_at_its_first_connect_and_nothing_on_the_nest() {
    let (machine, nest, mail) = build();
    fail_nest_reads(&nest);
    machine.refresh_epoch_schedule().await.unwrap();

    let cfg = mail.current();
    let key = cfg.msek.clone().expect("the account holds its key");
    let rows = mail.rows();
    assert!(
        rows.generations.contains_key(&MsekFingerprint::of(&key)),
        "the minted key is born in the ring"
    );
    assert!(
        cfg.prior_mseks.is_empty(),
        "the current key is never a prior"
    );
    assert!(!rows.has_mailbox(), "a key is not a mailbox");
    assert!(cfg.mail_enabled.is_none(), "the email flag stays unwritten");
    assert_eq!(mail.walks(), 1, "the mint walked first");
    assert_nest_untouched(&nest);

    machine.refresh_epoch_schedule().await.unwrap();
    assert_eq!(
        mail.current().msek,
        Some(key),
        "a later connect keeps the key"
    );
    assert_eq!(mail.walks(), 1, "a custody holding its key takes no walk");
    assert_nest_untouched(&nest);
}

/// **The connect-time mint is the machinery's write; an enable's is a
/// gesture** (`account-runtime.md` § Multi-instance concurrency → *A
/// runtime's own pump is a source of the notice too*, part 2): the mint at
/// first connect writes the key through the background door, so a page whose
/// calendar read was unresolved for want of the key re-runs when it lands
/// (`events.md` § Errors & edge cases → *No calendar to hold an event*,
/// ruling 2); an Enable that mints races no page — it repaints from its own
/// answer — and keeps the gesture door.
#[tokio::test]
async fn the_connect_time_mint_writes_the_key_through_the_background_door_and_an_enable_does_not() {
    let (machine, nest, mail) = build();
    fail_nest_reads(&nest);
    machine.refresh_epoch_schedule().await.unwrap();
    assert!(mail.current().msek.is_some(), "the key was minted");
    assert_eq!(
        mail.background_state_writes(),
        1,
        "the connect-time mint's state write is a background write"
    );

    let (enabling, _nest, enabled) = build();
    enabling
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "Default".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(b"enable-mints-the-key-24chars".to_vec()),
        })
        .await
        .expect("enable mail");
    assert!(
        enabled.current().msek.is_some(),
        "the enable minted the key"
    );
    assert_eq!(
        enabled.background_state_writes(),
        0,
        "an enable's writes are the page's gesture"
    );
}

/// **A walk finds a key another device minted** (ruling 3): a sibling whose
/// replica predates the mint walks, finds it, and mints none of its own.
#[tokio::test]
async fn a_walk_finds_the_key_another_device_minted() {
    let (first, nest, mail) = build();
    let sibling_store = mail.stale_replica();
    first.refresh_epoch_schedule().await.unwrap();
    use fauna_client_config::MailStore as _;
    mail.publish().await.unwrap();
    let key = mail.current().msek.unwrap();

    let sibling = machine_over(&nest, &sibling_store);
    assert!(!sibling.ensure_account_key().await.unwrap(), "no mint");
    assert_eq!(sibling_store.current().msek, Some(key));
    assert_eq!(
        sibling_store.rows().generations.len(),
        1,
        "one key, one ring row"
    );
}

/// **A wait is never a mint** (ruling 3): a walk that cannot be taken now, or
/// a custody that cannot be read, leaves the account without a key until the
/// next connect.
#[tokio::test]
async fn an_untakeable_walk_or_an_unread_custody_mints_nothing() {
    let (machine, nest, mail) = build();
    mail.refuse_next_walks(1);
    machine.refresh_epoch_schedule().await.unwrap();
    assert!(mail.current().msek.is_none(), "no walk, no mint");

    mail.refuse_reads_with("the account store is not up yet");
    assert!(machine.refresh_epoch_schedule().await.is_err());
    let unread = FakeMailStore::empty();
    unread.refuse_reads_with("the account store is not up yet");
    let other = machine_over(&nest, &unread);
    assert!(other.ensure_account_key().await.is_err());
    assert!(
        unread.rows().state.is_none(),
        "an unread custody is never minted over"
    );
    assert_nest_untouched(&nest);
}

/// **Two devices that had not met each mint; no minted key is lost**
/// (ruling 4): once their rows meet, both replicas fold to the one key the
/// state row's join keeps and the other as a prior — and a calendar body
/// sealed under the displaced key in the window opens through the ring
/// built from the merged fold.
#[tokio::test]
async fn two_unmet_mints_converge_and_the_displaced_key_still_opens_its_bodies() {
    let (a, nest_a, store_a) = build();
    let (b, nest_b, store_b) = build();
    a.refresh_epoch_schedule().await.unwrap();
    b.refresh_epoch_schedule().await.unwrap();
    let ka = store_a.current().msek.unwrap();
    let kb = store_b.current().msek.unwrap();
    assert_ne!(ka, kb, "each device sampled its own key");
    let body_a = seal_dav_body(DavKind::Calendar, b"BEGIN:VEVENT a", &ka.to_array()).unwrap();
    let body_b = seal_dav_body(DavKind::Calendar, b"BEGIN:VEVENT b", &kb.to_array()).unwrap();

    let meet = |ours: MailRows, theirs: &MailRows| {
        let mut rows = ours;
        if let Some(state) = &theirs.state {
            FakeMailStore::join_state_into(&mut rows, state);
        }
        for g in theirs.generations.values() {
            rows.join_generation(g.clone());
        }
        rows.config()
    };
    let on_a = meet(store_a.rows(), &store_b.rows());
    let on_b = meet(store_b.rows(), &store_a.rows());
    assert_eq!(on_a.msek, on_b.msek, "both replicas keep the one key");
    let winner = on_a.msek.clone().unwrap();
    let loser = if winner == ka { kb } else { ka };
    assert_eq!(
        on_a.prior_mseks,
        vec![loser.clone()],
        "the displaced key is a prior"
    );
    assert_eq!(on_b.prior_mseks, vec![loser]);

    let priors: Vec<[u8; 32]> = on_a.prior_mseks.iter().map(|k| k.to_array()).collect();
    let ring = DavRecipientKeys::from_mseks(DavKind::Calendar, &winner.to_array(), &priors);
    assert_eq!(ring.unseal(&body_a).unwrap(), b"BEGIN:VEVENT a");
    assert_eq!(ring.unseal(&body_b).unwrap(), b"BEGIN:VEVENT b");
    assert_nest_untouched(&nest_a);
    assert_nest_untouched(&nest_b);
}

/// **A rotation with no mailbox is custody-only** (ruling 6): it samples the
/// new key, keeps the outgoing one in the ring (retired at the rotation, not
/// at its mint) and calls no nest.
#[tokio::test]
async fn a_rotation_with_no_mailbox_is_custody_only() {
    let (machine, nest, mail) = build();
    machine.refresh_epoch_schedule().await.unwrap();
    let old = mail.current().msek.unwrap();
    let minted_at = mail.rows().generations[&MsekFingerprint::of(&old)].retired_at_unix;
    let machine = machine.with_retirement_clock(std::sync::Arc::new(move || minted_at + 100));
    fail_nest_reads(&nest);

    machine
        .dispatch(MailSettingsAction::StartRotation {
            excluded_credentials: Vec::new(),
        })
        .await
        .unwrap();

    let cfg = mail.current();
    let new = cfg.msek.clone().unwrap();
    assert_ne!(new, old, "a fresh key");
    assert_eq!(
        cfg.prior_mseks,
        vec![old.clone()],
        "the outgoing key stays in the ring"
    );
    assert_eq!(cfg.prior_msek_retired_at(&old), Some(minted_at + 100));
    assert!(cfg.pending_rotation.is_none(), "no sentinel left standing");
    assert!(!mail.rows().has_mailbox());
    assert_nest_untouched(&nest);
}

/// **The mailbox is the credential** (ruling 2): an account holding only its
/// key is a new user to the auto-enable (which mints the first credential
/// under that same key), and its link provisions nothing and answers no error.
#[tokio::test]
async fn a_key_alone_is_no_mailbox_to_the_auto_enable_or_the_link() {
    let (machine, nest, mail) = build();
    machine.refresh_epoch_schedule().await.unwrap();
    let key = mail.current().msek.unwrap();

    machine
        .dispatch(MailSettingsAction::ProvisionRelayMailbox)
        .await
        .expect("a link with no mailbox provisions nothing and answers no error");
    assert_nest_untouched(&nest);

    let password = machine
        .auto_enable_mail_for_new_user(true, false, true, "default".into())
        .await
        .unwrap();
    assert!(
        password.is_some(),
        "the account had no mailbox: the auto-enable mints one"
    );
    let rows = mail.rows();
    assert!(rows.has_mailbox());
    assert_eq!(
        mail.current().msek,
        Some(key),
        "provisioned under the key it held"
    );
    let again = machine
        .auto_enable_mail_for_new_user(true, false, true, "default".into())
        .await
        .unwrap();
    assert!(again.is_none(), "a mailbox is never re-minted");
}
