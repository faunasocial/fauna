//! The post-succession mail burn — the aftermath's MSEK leg.
//!
//! Authority: `docs/goal/behavior/succession-aftermath.md` § Re-key scope (the
//! MSEK row) + `docs/goal/behavior/mail-credentials.md` § Rotation and recovery
//! → *Succession*.
//!
//! What these pins are really guarding: until this leg runs, a succession does
//! not touch the mail plane at all, so the pre-succession seed holder — who read
//! the mail custody and therefore holds every credential secret — keeps reading the
//! *successor's* mail. Every assertion below is either "the exposure is closed"
//! or "closing it does not eat the successor's own fresh material".

use fauna_client_mail_settings::{
    CredentialKind, MailBurnOutcome, MailSettingsAction, MailSettingsMachine, SecretBytes,
    burn_mail_after_succession,
};
use fauna_core::identity::ActorId;

mod common;
use common::ACTOR;

const PREDECESSOR: ActorId = ActorId([0x77; 32]);
const OLDER_PREDECESSOR: ActorId = ActorId([0x66; 32]);

async fn enable_mail_with(machine: &MailSettingsMachine, name: &str, secret: u8) {
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: name.into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![secret; 32]),
        })
        .await
        .unwrap();
}

async fn add_credential(machine: &MailSettingsMachine, name: &str, secret: u8) {
    machine
        .dispatch(MailSettingsAction::AddCredential {
            display_name: name.into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![secret; 32]),
        })
        .await
        .unwrap();
}

/// The whole point of the leg: after it runs there is no path on which a
/// thief-held credential unwraps the post-succession MSEK. Three things have to
/// be true together — the key is new, the wrapped blobs that opened it are gone
/// (**both kinds**), and the successor's own recipient pubkey is what the MTA
/// will seal to next.
#[tokio::test]
async fn the_burn_rotates_the_msek_and_deletes_both_blob_kinds_for_every_credential() {
    let common::Fixture {
        machine,
        nest,
        config: _,
        mail,
    } = common::fixture();
    enable_mail_with(&machine, "default", 0xAA).await;
    add_credential(&machine, "iPhone", 0xBB).await;
    // The attested predecessors the aftermath hands the leg.
    let predecessors = [PREDECESSOR];
    let stolen_msek = mail.current().msek.unwrap();

    let outcome = burn_mail_after_succession(&machine, &predecessors)
        .await
        .unwrap();

    assert_eq!(outcome, MailBurnOutcome::Burned { credentials: 2 });
    let stored = mail.current();
    let fresh_msek = stored.msek.unwrap();
    assert_ne!(
        fresh_msek, stolen_msek,
        "the MSEK the predecessor's seed holder read must not survive the burn"
    );

    let s = nest.state();
    // Both kinds, for both credentials — the ordinary hard revoke leaves the
    // excluded credential's blobs resting, and that tolerance does not carry to
    // a holder who is known hostile (the submission-token half is what would
    // otherwise let them keep SENDING until the token expires).
    let revoked_mls: Vec<&String> = s.revoke_wrapped_mls.iter().map(|(_, id)| id).collect();
    let revoked_tokens: Vec<&String> = s.revoke_submission_token.iter().map(|(_, id)| id).collect();
    assert_eq!(
        revoked_mls.len(),
        2,
        "one wrapped-MSEK delete per credential"
    );
    assert_eq!(
        revoked_tokens.len(),
        2,
        "one submission-token delete per credential"
    );
    for id in &stored.credentials {
        assert!(revoked_mls.contains(&&id.credential_id));
        assert!(revoked_tokens.contains(&&id.credential_id));
    }

    // The degenerate rotation: survivors empty, so NO credential is re-wrapped
    // under the fresh key. A single re-wrap here would hand the thief MSEK′ and
    // reopen the whole hole while reporting success.
    let rewraps_after_burn = s
        .provision_wrapped_mls
        .iter()
        .filter(|blob| blob.index.1 == "default" || blob.index.1 == "iphone")
        .count();
    assert_eq!(
        rewraps_after_burn, 2,
        "only the two enable-time wraps; the burn must re-wrap nothing"
    );

    // The window the ceremony opened (handle resolves to a successor with no
    // registered pubkey) is closed under the SUCCESSOR's actor.
    let (actor, pubkey, _, _) = s.provision_recipient_pubkey.last().unwrap().clone();
    assert_eq!(actor, ACTOR);
    let (_, expected) = fauna_mls::wrapped_blob::derive_recipient_hpke_keypair(&fresh_msek);
    assert_eq!(
        pubkey, expected,
        "the re-registered pubkey must derive from the FRESH msek"
    );
}

/// § Succession: "every pre-succession credential row goes to *Compromised —
/// access revoked*". The row survives on purpose — it is the successor's list of
/// which mail apps to set up again — but the secret goes, because those bytes
/// are exactly what the predecessor's seed holder read.
#[tokio::test]
async fn burned_rows_survive_marked_and_emptied_of_their_secrets() {
    let common::Fixture {
        machine,
        nest: _nest,
        config: _,
        mail,
    } = common::fixture();
    enable_mail_with(&machine, "default", 0xAA).await;
    add_credential(&machine, "iPhone", 0xBB).await;
    // The attested predecessors the aftermath hands the leg.
    let predecessors = [PREDECESSOR];

    burn_mail_after_succession(&machine, &predecessors)
        .await
        .unwrap();

    let stored = mail.current();
    assert_eq!(
        stored.credentials.len(),
        2,
        "the rows stay — a list that simply emptied makes the user REMEMBER what to re-add"
    );
    assert_eq!(stored.live_credentials().count(), 0);
    for row in &stored.credentials {
        let burn = row
            .burned
            .as_ref()
            .expect("every pre-succession row is marked");
        assert_eq!(burn.predecessor, PREDECESSOR);
        assert!(
            row.secret.as_ref().is_empty(),
            "a known-compromised secret must not stay at rest"
        );
    }
}

/// The idempotency device. A second pass must not rotate again — and once the
/// burn is recorded, credentials the successor mints afterwards are their own
/// fresh material, which the predecessor never saw.
#[tokio::test]
async fn a_second_pass_burns_nothing_and_spares_the_successors_own_new_credential() {
    let common::Fixture {
        machine,
        nest,
        config: _,
        mail,
    } = common::fixture();
    enable_mail_with(&machine, "default", 0xAA).await;
    // The attested predecessors the aftermath hands the leg.
    let predecessors = [PREDECESSOR];
    burn_mail_after_succession(&machine, &predecessors)
        .await
        .unwrap();
    let post_burn_msek = mail.current().msek.unwrap();

    // The successor sets their mail app up again, as the done-line tells them to.
    add_credential(&machine, "iPhone", 0xCC).await;
    let deletes_before = nest.state().revoke_wrapped_mls.len();

    let outcome = burn_mail_after_succession(&machine, &predecessors)
        .await
        .unwrap();

    assert_eq!(outcome, MailBurnOutcome::NothingToBurn);
    let stored = mail.current();
    assert_eq!(
        stored.msek.unwrap(),
        post_burn_msek,
        "a second pass must not rotate the key the successor is now using"
    );
    assert_eq!(nest.state().revoke_wrapped_mls.len(), deletes_before);
    let fresh = stored
        .credentials
        .iter()
        .find(|c| c.credential_id == "iphone")
        .expect("the re-added credential is present");
    assert!(
        fresh.burned.is_none(),
        "the successor's own post-burn credential is not predecessor-era material"
    );
    assert!(!fresh.secret.as_ref().is_empty());
}

/// The arm that is easiest to get wrong by skipping: a successor with mail
/// disabled has nothing to revoke, but the burn must still be RECORDED. Without
/// that record the leg stays owed forever, and the credential minted by a later
/// "Enable mail" — a secret the predecessor never saw — is burned as if stolen.
///
/// ⚠ The later enable writes the state row (the fresh MSEK), so this also pins
/// that the burn record survives that write — the state row's join unions the
/// burns, and the enable restates none.
#[tokio::test]
async fn a_successor_with_no_mail_records_the_burn_so_a_later_enable_survives_it() {
    let common::Fixture {
        machine,
        nest,
        config: _,
        mail,
    } = common::fixture();
    // The attested predecessors the aftermath hands the leg.
    let predecessors = [PREDECESSOR];

    let outcome = burn_mail_after_succession(&machine, &predecessors)
        .await
        .unwrap();

    assert_eq!(outcome, MailBurnOutcome::NoMailMaterial);
    assert!(
        mail.current().burned_for(&PREDECESSOR),
        "the burn is recorded even with no mail plane to burn"
    );

    // Later, the successor turns mail on for the first time.
    enable_mail_with(&machine, "default", 0xDD).await;
    let enabled_msek = mail.current().msek.unwrap();
    let deletes_before = nest.state().revoke_wrapped_mls.len();

    let outcome = burn_mail_after_succession(&machine, &predecessors)
        .await
        .unwrap();

    assert_eq!(outcome, MailBurnOutcome::NothingToBurn);
    assert_eq!(
        mail.current().msek.unwrap(),
        enabled_msek,
        "the mail the successor just enabled must not be burned"
    );
    assert_eq!(nest.state().revoke_wrapped_mls.len(), deletes_before);
    assert!(
        mail.current()
            .credentials
            .iter()
            .all(|c| c.burned.is_none())
    );
}

/// The overwhelmingly common call: an identity that never succeeded. It must
/// cost nothing and touch nothing — this runs at every sign-in on every device.
#[tokio::test]
async fn an_ordinary_identity_burns_nothing() {
    let common::Fixture {
        machine,
        nest,
        config: _,
        mail,
    } = common::fixture();
    enable_mail_with(&machine, "default", 0xAA).await;
    let before = mail.current().msek;
    let deletes_before = nest.state().revoke_wrapped_mls.len();

    let outcome = burn_mail_after_succession(&machine, &[]).await.unwrap();

    assert_eq!(outcome, MailBurnOutcome::NothingToBurn);
    assert_eq!(mail.current().msek, before);
    assert_eq!(nest.state().revoke_wrapped_mls.len(), deletes_before);
}

/// A twice-succeeded chain reaching the leg with two unburned predecessors
/// records BOTH — otherwise the second sign-in re-burns for the one it skipped,
/// eating whatever the successor has re-added by then.
#[tokio::test]
async fn every_owed_predecessor_is_recorded_in_one_pass() {
    let common::Fixture {
        machine,
        nest: _nest,
        config: _,
        mail,
    } = common::fixture();
    enable_mail_with(&machine, "default", 0xAA).await;
    // The attested predecessors the aftermath hands the leg.
    let predecessors = [OLDER_PREDECESSOR, PREDECESSOR];

    burn_mail_after_succession(&machine, &predecessors)
        .await
        .unwrap();

    let stored = mail.current();
    assert!(stored.burned_for(&OLDER_PREDECESSOR));
    assert!(stored.burned_for(&PREDECESSOR));
    assert_eq!(
        burn_mail_after_succession(&machine, &predecessors)
            .await
            .unwrap(),
        MailBurnOutcome::NothingToBurn
    );
}
