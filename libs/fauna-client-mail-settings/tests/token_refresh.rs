//! Tests for the **submission-token refresh** — the background re-mint that
//! keeps every credential's outbound `SubmissionToken` well inside its 30-day
//! lifetime, so a mail app keeps sending month after month with nothing
//! re-entered.
//!
//! Authority: `docs/goal/behavior/mail-credentials.md` § Architectural rules,
//! rule 10 ("SubmissionToken refreshes on a 7-day cadence … any online client
//! refreshes"), and § Trigger taxonomy, the "Submission-token refresh" row.
//!
//! Every test passes the refresh its `now` — the cadence is a week, and a
//! test never sleeps it out (convention 14).

use fauna_client_mail_settings::testing::FakeSigner;
use fauna_client_mail_settings::token_refresh::SUBMISSION_TOKEN_REFRESH_INTERVAL_SECS;
use fauna_client_mail_settings::{CredentialKind, MailSettingsAction, SecretBytes};
use fauna_core::data::Timestamp;
use fauna_mls::wrapped_blob::{CredentialInput, unseal_submission_token};

mod common;
use common::{SIGNER_SEED, build};

const DAY: u64 = 86_400;
const LIFETIME: u64 = 30 * DAY;

fn enable() -> MailSettingsAction {
    // OAUTHBEARER → HKDF (fast).
    MailSettingsAction::EnableMail {
        display_name: "default".into(),
        kind: CredentialKind::OAuthBearer,
        secret: SecretBytes::from(vec![0xDE; 32]),
    }
}

fn add(name: &str, byte: u8) -> MailSettingsAction {
    MailSettingsAction::AddCredential {
        display_name: name.into(),
        kind: CredentialKind::OAuthBearer,
        secret: SecretBytes::from(vec![byte; 32]),
    }
}

/// Every path that provisions a token records its mint on the credential's
/// row — the stamp the refresh measures the cadence from.
#[tokio::test]
async fn every_provisioned_token_stamps_its_credential_row() {
    let (machine, _nest, mail) = build();
    let before = Timestamp::now_secs() as u64;
    machine.dispatch(enable()).await.expect("enable");
    machine.dispatch(add("phone", 0xA1)).await.expect("add");
    let after = Timestamp::now_secs() as u64;
    let rows = mail.rows();
    assert_eq!(rows.credentials.len(), 2);
    for c in rows.credentials.values() {
        let minted = c
            .token_minted_at
            .unwrap_or_else(|| panic!("{} has no token-mint stamp", c.credential_id));
        assert!(
            (before..=after).contains(&minted),
            "{minted} outside {before}..={after}"
        );
    }
}

/// A token younger than the cadence is left alone; one at or past it is
/// re-minted under the refresh's `now` — a fresh 30-day lifetime, provisioned
/// as an atomic replace — and the row's stamp moves with it, so the next pass
/// at the same instant re-mints nothing.
#[tokio::test]
async fn a_token_past_the_cadence_is_reminted_and_a_younger_one_is_not() {
    let (machine, nest, mail) = build();
    machine.dispatch(enable()).await.expect("enable");
    let minted = mail.rows().credentials["default"]
        .token_minted_at
        .expect("enable stamps the row");
    let provisioned = nest.state().provision_submission_token.len();

    let younger = minted + SUBMISSION_TOKEN_REFRESH_INTERVAL_SECS - 1;
    assert_eq!(machine.refresh_submission_tokens(younger).await.unwrap(), 0);
    assert_eq!(nest.state().provision_submission_token.len(), provisioned);

    let due = minted + SUBMISSION_TOKEN_REFRESH_INTERVAL_SECS;
    assert_eq!(machine.refresh_submission_tokens(due).await.unwrap(), 1);
    let blob = nest
        .state()
        .provision_submission_token
        .last()
        .cloned()
        .expect("a re-mint provisions");
    assert_eq!(
        nest.state().provision_submission_token.len(),
        provisioned + 1
    );
    let token = unseal_submission_token(
        &blob,
        &CredentialInput::OauthBearer(&[0xDE; 32]),
        &FakeSigner::new(SIGNER_SEED).verifying_key(),
    )
    .expect("the re-minted token unseals under the credential");
    assert_eq!(token.credential_id, "default");
    assert_eq!(token.issued_at, due);
    assert_eq!(token.expires_at, due + LIFETIME);
    assert_eq!(
        mail.rows().credentials["default"].token_minted_at,
        Some(due)
    );

    assert_eq!(machine.refresh_submission_tokens(due).await.unwrap(), 0);
}

/// Only an email mailbox's live credentials hold a token: a revoked
/// credential is never re-minted, and nothing is re-minted once mail is
/// disabled. A row with no stamp (unknown) is owed a re-mint at once.
#[tokio::test]
async fn only_live_credentials_of_an_email_mailbox_are_reminted() {
    let (machine, nest, mail) = build();
    machine.dispatch(enable()).await.expect("enable");
    machine.dispatch(add("phone", 0xA1)).await.expect("add");
    machine
        .dispatch(MailSettingsAction::RevokeCredential {
            credential_id: "phone".into(),
        })
        .await
        .expect("revoke");
    let later = Timestamp::now_secs() as u64 + 8 * DAY;
    let before = nest.state().provision_submission_token.len();
    assert_eq!(machine.refresh_submission_tokens(later).await.unwrap(), 1);
    assert_eq!(nest.state().provision_submission_token.len(), before + 1);
    assert!(mail.rows().credentials["phone"].token_minted_at.is_some());
    assert!(
        mail.rows().credentials["phone"].token_minted_at < Some(later),
        "the revoked row is never re-stamped"
    );

    machine
        .dispatch(MailSettingsAction::DisableMail)
        .await
        .expect("disable");
    let much_later = later + 8 * DAY;
    assert_eq!(
        machine.refresh_submission_tokens(much_later).await.unwrap(),
        0
    );
}
