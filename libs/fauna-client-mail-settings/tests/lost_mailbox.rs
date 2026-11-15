//! **A nest that lost the mailbox gets it back from the custody.**
//! Authority: `docs/goal/behavior/mail-credentials.md` § A nest that lost the
//! mailbox (factory reset, rebuilt box): the nest-side half of a mailbox is a
//! projection of the account's mail custody. A factory reset and a reclaim, a
//! box rebuilt over an empty database or a replaced home nest keep the custody
//! — the MSEK, the credential rows with their generation markers, the email
//! flag, a standing sentinel — and lose the snapshot, the registration, each
//! credential's wrapped MSEK and submission token and the per-actor flag. The
//! reading is "registration absent while the custody holds the MSEK and a live
//! credential naming a generation"; the restore runs from one shared function
//! with three callers (claim, connect, hydrate).
//!
//! The wiped box is a fresh [`FakeNestClient`] beside the same
//! [`FakeMailStore`]: exactly what the account plane's bind leg leaves.

use fauna_client_mail_settings::machine::NestClient;
use fauna_client_mail_settings::testing::{FakeMailStore, FakeNestClient};
use fauna_client_mail_settings::{
    CredentialKind, MailSettingsAction, MailSettingsMachine, SecretBytes,
};
use fauna_core::data::MsekFingerprint;
use fauna_core::mail_rows::MailRotationSentinel;
use fauna_core::secret::SecretArray32;

mod common;
use common::{ACTOR, build, machine_over};

async fn enable(machine: &MailSettingsMachine) {
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAA; 32]),
        })
        .await
        .unwrap();
}

async fn add(machine: &MailSettingsMachine, name: &str, byte: u8) {
    machine
        .dispatch(MailSettingsAction::AddCredential {
            display_name: name.into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![byte; 32]),
        })
        .await
        .unwrap();
}

/// A completed two-credential email mailbox on its first box, then the box
/// wiped: the custody, a wiped nest, and a machine over both.
async fn a_mailbox_whose_box_was_wiped() -> (MailSettingsMachine, FakeNestClient, FakeMailStore) {
    let (machine, _old_nest, mail) = build();
    enable(&machine).await;
    add(&machine, "iPhone", 0xBB).await;
    let wiped = FakeNestClient::new();
    let restored = machine_over(&wiped, &mail);
    (restored, wiped, mail)
}

/// The credential ids and secrets the custody holds, sorted — what must
/// survive the restore unchanged (the user's mail apps keep their passwords).
fn credential_ids_and_secrets(mail: &FakeMailStore) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<(String, Vec<u8>)> = mail
        .current()
        .credentials
        .iter()
        .map(|c| (c.credential_id.clone(), c.secret.to_vec()))
        .collect();
    v.sort();
    v
}

/// The nest-side half is whole on `nest` for every live credential, under
/// the custody's standing MSEK: snapshot, the registration derived from it,
/// a wrapped MSEK and a submission token per credential, each row naming
/// the standing generation, and the per-actor mail flag on.
async fn assert_nest_holds_the_mailbox(nest: &FakeNestClient, mail: &FakeMailStore) {
    let stored = mail.current();
    assert!(stored.pending_rotation.is_none(), "no sentinel stands");
    assert!(stored.is_mail_enabled(), "email stays on");
    let msek = stored.msek.clone().expect("the custody keeps its MSEK");
    let registered = nest
        .fetch_own_recipient_pubkey(ACTOR)
        .await
        .unwrap()
        .expect("the registration is back on the nest");
    assert!(
        registered.derived_from(&msek.to_array()),
        "the registration derives from the standing MSEK"
    );
    let fp = MsekFingerprint::of(&msek);
    let s = nest.state();
    assert!(!s.provision_mls_snapshot.is_empty(), "the snapshot is back");
    for c in stored.credentials.iter().filter(|c| c.burned.is_none()) {
        assert_eq!(
            c.wrapped_under,
            Some(fp),
            "{} names the standing generation",
            c.credential_id
        );
        assert!(
            s.provision_wrapped_mls
                .iter()
                .any(|b| b.index.1 == c.credential_id),
            "{}'s wrapped MSEK is back",
            c.credential_id
        );
        assert!(
            s.provision_submission_token
                .iter()
                .any(|t| t.index.1 == c.credential_id),
            "{}'s submission token is back",
            c.credential_id
        );
    }
    assert_eq!(
        s.actor_mail_enabled,
        Some(true),
        "the per-actor mail flag is on"
    );
}

#[tokio::test]
async fn the_claim_restores_a_completed_mailbox_onto_a_wiped_box() {
    let (machine, wiped, mail) = a_mailbox_whose_box_was_wiped().await;
    let before = credential_ids_and_secrets(&mail);
    let msek = mail.current().msek;

    let held = machine
        .restore_mailbox_at_claim(true)
        .await
        .expect("the claim-time restore");
    assert!(
        held,
        "the custody held a mailbox: restored, never minted over"
    );

    assert_nest_holds_the_mailbox(&wiped, &mail).await;
    assert_eq!(
        credential_ids_and_secrets(&mail),
        before,
        "same credentials"
    );
    assert_eq!(mail.current().msek, msek, "same MSEK — nothing re-minted");
    assert_eq!(
        wiped.state().set_mail_enabled,
        vec![true],
        "the claim chose email: the deployment subsystem goes on"
    );
}

#[tokio::test]
async fn the_claim_without_email_restores_but_leaves_the_deployment_flag() {
    let (machine, wiped, mail) = a_mailbox_whose_box_was_wiped().await;
    assert!(machine.restore_mailbox_at_claim(false).await.unwrap());
    assert_nest_holds_the_mailbox(&wiped, &mail).await;
    assert!(
        wiped.state().set_mail_enabled.is_empty(),
        "the deployment-wide flag is the admin's choice on the new box"
    );
}

#[tokio::test]
async fn the_hydrate_restores_a_wiped_box_on_its_own() {
    let (machine, wiped, mail) = a_mailbox_whose_box_was_wiped().await;
    let before = credential_ids_and_secrets(&mail);
    machine.hydrate().await.expect("hydrate");
    assert_nest_holds_the_mailbox(&wiped, &mail).await;
    assert_eq!(credential_ids_and_secrets(&mail), before);
    assert!(
        wiped.state().set_mail_enabled.is_empty(),
        "a non-claim caller never sets the deployment-wide flag"
    );
    let snap = machine.snapshot();
    assert!(snap.error.is_none(), "no error on the page");
    assert!(snap.pending_rotation.is_none());
}

#[tokio::test]
async fn the_connect_time_refresh_restores_a_wiped_box_on_its_own() {
    let (machine, wiped, mail) = a_mailbox_whose_box_was_wiped().await;
    machine
        .refresh_epoch_schedule()
        .await
        .expect("the connect-time refresh");
    assert_nest_holds_the_mailbox(&wiped, &mail).await;
    assert!(wiped.state().set_mail_enabled.is_empty());
}

#[tokio::test]
async fn a_first_enable_sentinel_carried_onto_a_wiped_box_resumes_at_the_claim() {
    // The first box: the enable stopped after its wrap (marker written),
    // before its submission token — the sentinel stands.
    let (machine, old_nest, mail) = build();
    old_nest.state().fail_provision_submission_token_for_n_calls = 1;
    let enabled = machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAA; 32]),
        })
        .await;
    assert!(enabled.is_err(), "the refused token stops the enable");
    let stopped = mail.current();
    assert!(stopped.pending_rotation.is_some(), "the sentinel stands");
    assert!(
        stopped.credentials[0].wrapped_under.is_some(),
        "stopped after the wrap marker"
    );

    let wiped = FakeNestClient::new();
    let restored = machine_over(&wiped, &mail);
    assert!(restored.restore_mailbox_at_claim(true).await.unwrap());

    assert_nest_holds_the_mailbox(&wiped, &mail).await;
    assert_eq!(mail.current().credentials.len(), 1, "no second mint");
    assert_eq!(wiped.state().set_mail_enabled.last(), Some(&true));
}

#[tokio::test]
async fn a_rotation_sentinel_carried_onto_a_wiped_box_finishes_onto_the_staged_msek() {
    let (machine, _old_nest, mail) = build();
    enable(&machine).await;
    add(&machine, "iPhone", 0xBB).await;
    let old_msek = mail.current().msek.unwrap();
    let staged = [0x5E; 32];
    // The rotation stopped mid-loop on the first box: the sentinel names the
    // staged MSEK and one row already names its generation.
    mail.mutate(|rows| {
        rows.state.as_mut().unwrap().pending_rotation = Some(MailRotationSentinel {
            new_msek: staged.into(),
        });
        let first = rows.credentials.values_mut().next().unwrap();
        first.wrapped_under = Some(MsekFingerprint::of(&SecretArray32::from(staged)));
    });

    let wiped = FakeNestClient::new();
    let restored = machine_over(&wiped, &mail);
    restored.hydrate().await.expect("hydrate");

    let stored = mail.current();
    assert_eq!(
        stored.msek.as_ref().map(|m| m.to_array()),
        Some(staged),
        "the rotation finished onto its staged MSEK"
    );
    assert_ne!(stored.msek, Some(old_msek));
    assert_nest_holds_the_mailbox(&wiped, &mail).await;
    assert!(
        wiped.state().set_mail_enabled.is_empty(),
        "the hydrate never sets the deployment-wide flag"
    );
}

#[tokio::test]
async fn a_first_enable_in_flight_is_not_read_as_a_lost_mailbox() {
    // Custody holds the MSEK and a credential owed its wrap (no marker): the
    // registration is absent because the enable has not reached it yet.
    let (machine, nest, mail) = build();
    enable(&machine).await;
    mail.mutate(|rows| {
        for c in rows.credentials.values_mut() {
            c.wrapped_under = None;
        }
    });
    let wiped = FakeNestClient::new();
    let other = machine_over(&wiped, &mail);
    assert!(
        !other.restore_lost_mailbox().await.unwrap(),
        "no marker: not a lost mailbox"
    );
    assert!(wiped.state().provision_mls_snapshot.is_empty());
    assert!(wiped.state().provision_submission_token.is_empty());
    drop(nest);
}

#[tokio::test]
async fn a_healthy_nest_moves_nothing() {
    let (machine, nest, mail) = build();
    enable(&machine).await;
    let (snapshots, wraps, tokens) = {
        let s = nest.state();
        (
            s.provision_mls_snapshot.len(),
            s.provision_wrapped_mls.len(),
            s.provision_submission_token.len(),
        )
    };
    let markers: Vec<_> = mail
        .current()
        .credentials
        .iter()
        .map(|c| c.wrapped_under)
        .collect();
    assert!(!machine.restore_lost_mailbox().await.unwrap());
    assert!(!machine.restore_mailbox_at_claim(true).await.unwrap());
    let s = nest.state();
    assert_eq!(s.provision_mls_snapshot.len(), snapshots);
    assert_eq!(s.provision_wrapped_mls.len(), wraps);
    assert_eq!(s.provision_submission_token.len(), tokens);
    drop(s);
    let after: Vec<_> = mail
        .current()
        .credentials
        .iter()
        .map(|c| c.wrapped_under)
        .collect();
    assert_eq!(after, markers, "no marker cleared on a healthy nest");
}

#[tokio::test]
async fn no_mailbox_in_custody_is_not_held() {
    let (machine, nest, _mail) = build();
    assert!(!machine.restore_lost_mailbox().await.unwrap());
    assert!(!machine.restore_mailbox_at_claim(true).await.unwrap());
    assert!(nest.state().provision_mls_snapshot.is_empty());
}
