//! End-to-end dispatch tests for `MailSettingsMachine` against the
//! in-memory fakes in `crate::testing`. Covers the three main
//! user-driven dispatches: EnableMail, AddCredential,
//! RevokeCredential.

use std::sync::Arc;

use fauna_client_mail_settings::error::DispatchError;
use fauna_client_mail_settings::testing::{
    FakeMailStore, FakeNestClient, FakeSigner, FakeSuccessionLedgerStore,
};
use fauna_client_mail_settings::{
    CredentialKind, MailSettingsAction, MailSettingsMachine, MuaInstructions, NestError,
    SecretBytes,
};

const ACTOR: [u8; 32] = [0x11; 32];
const SIGNER_SEED: [u8; 32] = [0x22; 32];

fn build_machine() -> (MailSettingsMachine, FakeNestClient, FakeMailStore) {
    let nest = FakeNestClient::new();
    let mail = FakeMailStore::empty();
    let signer = FakeSigner::new(SIGNER_SEED);
    let machine = MailSettingsMachine::new(
        ACTOR,
        Arc::new(nest.clone()),
        Arc::new(FakeSuccessionLedgerStore::empty(
            fauna_core::identity::ActorId(ACTOR),
        )),
        Arc::new(mail.clone()),
        Arc::new(signer),
        MuaInstructions::placeholder(),
    );
    (machine, nest, mail)
}

#[tokio::test]
async fn reveal_credential_secret_round_trips_from_the_mail_store() {
    let (machine, _nest, _mail) = build_machine();
    let pw = b"round-trip-plain-pw-24chars!"; // the bytes the client sealed under
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "Default".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(pw.to_vec()),
        })
        .await
        .expect("enable mail");

    // The default credential's secret is recoverable from the client's own
    // mail custody — the reveal returns exactly what was sealed (so a user can
    // reconfigure a MUA without revoke + re-add). credential_id "Default" →
    // "default".
    let revealed = machine
        .reveal_credential_secret("default".to_string())
        .await
        .expect("reveal default");
    assert_eq!(revealed.as_str().as_bytes(), pw);

    // An unknown credential_id is rejected, not silently empty.
    let err = machine
        .reveal_credential_secret("no-such-credential".to_string())
        .await
        .expect_err("unknown credential must error");
    assert!(
        matches!(err, DispatchError::UnknownCredential(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn enable_mail_with_generated_password_mints_and_returns_revealable_password() {
    // The shared auto-mint path (onboarding auto-complete + new-user
    // auto-enable): enable mail with a generated password, returned once.
    let (machine, _nest, _mail) = build_machine();

    let pw = machine
        .enable_mail_with_generated_password("Default".into())
        .await
        .expect("auto-mint mailbox");

    // Mail is enabled (a credential + MSEK were minted).
    assert!(machine.snapshot().enabled, "mail enabled after auto-mint");

    // The returned password has the generated bridge-password shape (24 chars,
    // a-zA-Z0-9) — surfaced once for the user to copy into a MUA.
    let s = pw.as_str();
    assert_eq!(s.len(), 24, "24-char generated password");
    assert!(
        s.chars().all(|c| c.is_ascii_alphanumeric()),
        "alphanumeric: {s}"
    );

    // The same password was actually sealed into the credential — reveal
    // returns exactly what the helper returned, so the surfaced password works
    // for an IMAP login. credential_id "Default" → "default".
    let revealed = machine
        .reveal_credential_secret("default".to_string())
        .await
        .expect("reveal default");
    assert_eq!(
        revealed.as_str(),
        s,
        "revealed credential must equal the returned password"
    );

    // Idempotency guard (`onboarding.md:142`): a second auto-mint on an
    // already-enabled actor errors rather than minting a second mailbox.
    let err = machine
        .enable_mail_with_generated_password("Second".into())
        .await
        .expect_err("already enabled");
    assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
}

/// A device whose page hydrated before the custody's rows reached its store
/// (a fresh sign-in on a fleet-only, tip-sealed kind) offers Enable over a
/// mailbox that is already on. The refusal stands — no second mailbox, and no
/// secret reported as a password no credential holds — but the machine
/// refreshes its snapshot from the custody first, so the page stops offering
/// an Enable that can only fail.
#[tokio::test]
async fn enable_over_an_enabled_custody_refreshes_the_stale_snapshot_before_refusing() {
    let (first, _nest, mail) = build_machine();
    first
        .enable_mail_with_generated_password("Default".into())
        .await
        .expect("the first device enables mail");

    // A second machine over the SAME custody that never hydrated: its snapshot
    // still reads the never-enabled default.
    let second = MailSettingsMachine::new(
        ACTOR,
        Arc::new(FakeNestClient::new()),
        Arc::new(FakeSuccessionLedgerStore::empty(
            fauna_core::identity::ActorId(ACTOR),
        )),
        Arc::new(mail.clone()),
        Arc::new(FakeSigner::new(SIGNER_SEED)),
        MuaInstructions::placeholder(),
    );
    assert!(
        !second.snapshot().enabled,
        "precondition: the stale page reads disabled"
    );

    let err = second
        .enable_mail_with_generated_password("Second".into())
        .await
        .expect_err("an enabled custody refuses a second enable");
    assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
    assert!(
        second.snapshot().enabled,
        "the refusal must leave the page showing the enabled mailbox it found"
    );
    assert_eq!(
        second.snapshot().credentials.len(),
        1,
        "no second credential minted"
    );
}

#[tokio::test]
async fn enable_caldav_mailbox_mints_read_only_material_without_submission_or_toggle() {
    // CalDAV-only enablement (`caldav-server.md` § Independent enablement): a user
    // enabling calendar without email gets the *shared* mailbox key material so
    // their `bridge_caldav_*` store seals — but with TWO deliberate differences
    // from `enable_mail`: (1) NO outbound submission token (the read-only recipe;
    // CalDAV needs no outbound mail), and (2) NO deployment mail-toggle flip (that
    // Admin toggle is the BridgeApprovalMachine's `SetCalDavEnabled`, fired
    // separately). Minting the MSEK does NOT make the user email-reachable (an
    // alias is a separate concern), so they stay genuinely mailbox-less.
    let (machine, nest, mail) = build_machine();

    let pw = machine
        .enable_caldav_mailbox_with_generated_password("Default".into())
        .await
        .expect("enable caldav mailbox");

    // Same read-side material `enable_mail` provisions...
    {
        let s = nest.state();
        assert_eq!(s.provision_mls_snapshot.len(), 1, "snapshot blob");
        assert_eq!(s.provision_wrapped_mls.len(), 1, "wrapped-MSEK blob");
        assert_eq!(
            s.provision_recipient_pubkey.len(),
            1,
            "recipient pubkey registered"
        );
        // ...but NOT the outbound submission token, and NOT the deployment toggle.
        assert_eq!(
            s.provision_submission_token.len(),
            0,
            "NO submission token — CalDAV needs no outbound mail (read-only recipe)"
        );
        assert!(
            s.set_mail_enabled.is_empty(),
            "NO deployment mail-toggle flip (owned by BridgeApprovalMachine::SetCalDavEnabled)"
        );
    }

    // The shared MSEK is populated (the calendar store seals under it) and the
    // generated password is the sealed credential — usable for a stock CalDAV
    // client AUTH. credential_id "Default" → "default".
    let stored = mail.current();
    assert!(stored.msek.is_some(), "shared MSEK populated");
    assert_eq!(stored.credentials.len(), 1, "one default read credential");
    let revealed = machine
        .reveal_credential_secret("default".to_string())
        .await
        .expect("reveal default");
    assert_eq!(
        revealed.as_str(),
        pw.as_str(),
        "revealed credential equals the returned password"
    );
}

#[tokio::test]
async fn enable_caldav_mailbox_is_idempotent_when_mailbox_already_provisioned() {
    // The MSEK is shared across email + CalDAV. If a mailbox already exists (here,
    // email was enabled first), enabling CalDAV must NOT re-mint a fresh MSEK —
    // that would silently break the existing mail + calendar store. It is a no-op.
    let (machine, nest, mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "Default".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(b"existing-mail-password-bytes".to_vec()),
        })
        .await
        .expect("enable mail");
    let msek_after_mail = mail.current().msek;
    let snapshots_after_mail = nest.state().provision_mls_snapshot.len();

    machine
        .enable_caldav_mailbox_with_generated_password("Calendar".into())
        .await
        .expect("enable caldav (idempotent on an existing mailbox)");

    assert_eq!(
        mail.current().msek,
        msek_after_mail,
        "shared MSEK preserved — CalDAV-enable did not re-mint"
    );
    assert_eq!(
        nest.state().provision_mls_snapshot.len(),
        snapshots_after_mail,
        "no new blob provisioning on the idempotent CalDAV-enable"
    );
    assert_eq!(
        mail.current().credentials.len(),
        1,
        "no extra credential added"
    );
    // The CalDAV flag IS recorded on the existing (email) mailbox — so a later
    // "Disable mail" preserves the shared MSEK the calendar now needs — while
    // email stays enabled.
    let stored = mail.current();
    assert!(stored.caldav_enabled, "CalDAV flag recorded");
    assert!(stored.is_mail_enabled(), "email stays enabled");
    assert!(
        machine.snapshot().enabled,
        "snapshot still shows mail enabled"
    );
}

#[tokio::test]
async fn caldav_only_mailbox_renders_mail_disabled_in_snapshot() {
    // The snapshot's `enabled` is **email**, NOT "has an MSEK": a CalDAV-only
    // actor holds the shared MSEK (the calendar store seals under it) but has no
    // email, so the mail-settings page must render "Mail disabled". Without this
    // the user would see a "Disable mail" affordance that, if clicked, would
    // clear the MSEK their calendar depends on.
    let (machine, _nest, mail) = build_machine();
    machine
        .enable_caldav_mailbox_with_generated_password("Default".into())
        .await
        .expect("enable caldav mailbox");

    assert!(
        !machine.snapshot().enabled,
        "CalDAV-only ⇒ snapshot reports mail DISABLED despite the shared MSEK"
    );
    let stored = mail.current();
    assert!(stored.msek.is_some(), "the shared MSEK is present");
    assert!(stored.caldav_enabled, "CalDAV is enabled");
    assert!(!stored.is_mail_enabled(), "email is not enabled");
    // The default read credential is still surfaced (a CalDAV/IMAP MUA AUTHs with
    // it) — disabled-mail ≠ no-credential.
    assert_eq!(machine.snapshot().credentials.len(), 1);
}

#[tokio::test]
async fn enable_caldav_then_enable_mail_upgrades_additively_reusing_msek_and_credential() {
    // (a) The CalDAV→email upgrade. A CalDAV-only mailbox already holds the
    // shared MSEK + `default` read credential (read-only recipe — no submission
    // token). Enabling email must be *additive*: reuse them and provision only
    // the missing outbound submission token + flip the deployment toggle — NOT
    // error ("already enabled") and NOT mint a second mailbox/credential (one
    // shared bridge password serves IMAP + SMTP + CalDAV).
    let (machine, nest, mail) = build_machine();
    let caldav_pw = machine
        .enable_caldav_mailbox_with_generated_password("Default".into())
        .await
        .expect("enable caldav mailbox");
    let msek_after_caldav = mail.current().msek;
    assert_eq!(
        nest.state().provision_submission_token.len(),
        0,
        "read-only recipe minted NO submission token"
    );

    // Now enable email. The passed credential is intentionally a throwaway — the
    // upgrade reuses the CalDAV credential, so this one must be ignored.
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "ignored".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(b"throwaway-should-be-ignored".to_vec()),
        })
        .await
        .expect("enabling email on a CalDAV-only mailbox upgrades additively");

    let stored = mail.current();
    assert_eq!(
        stored.msek, msek_after_caldav,
        "the shared MSEK is REUSED — no second mailbox minted"
    );
    assert_eq!(
        stored.credentials.len(),
        1,
        "the CalDAV credential is REUSED — no second credential added"
    );
    assert!(stored.is_mail_enabled(), "email now enabled");
    assert!(stored.caldav_enabled, "CalDAV stays enabled");
    assert!(
        machine.snapshot().enabled,
        "snapshot now shows mail enabled"
    );

    {
        let s = nest.state();
        assert_eq!(
            s.provision_submission_token.len(),
            1,
            "the upgrade provisioned the missing outbound submission token"
        );
        assert_eq!(
            s.set_mail_enabled,
            vec![true],
            "the upgrade flipped the deployment-wide mail subsystem on"
        );
    }

    // The reused credential is still the CalDAV one — reveal returns the CalDAV
    // password, not the throwaway, so the user's existing MUA keeps working.
    let revealed = machine
        .reveal_credential_secret("default".to_string())
        .await
        .expect("reveal default");
    assert_eq!(
        revealed.as_str(),
        caldav_pw.as_str(),
        "the shared bridge password is unchanged by enabling email"
    );
}

#[tokio::test]
async fn disable_mail_preserves_msek_and_read_credential_when_caldav_enabled() {
    // (b) Disabling email on an actor who ALSO has CalDAV must NOT clear the
    // shared MSEK or revoke the read credential — that AEAD-unwrap blob also
    // authenticates CalDAV and the calendar store seals under the MSEK. Only the
    // outbound submission tokens are revoked; CalDAV keeps working.
    let (machine, nest, mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "Default".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(b"shared-bridge-password-bytes".to_vec()),
        })
        .await
        .expect("enable mail");
    machine
        .enable_caldav_mailbox_with_generated_password("Calendar".into())
        .await
        .expect("enable caldav (idempotent — records the flag)");
    let msek_before_disable = mail.current().msek;

    machine
        .dispatch(MailSettingsAction::DisableMail)
        .await
        .expect("disable mail while CalDAV stays on");

    let stored = mail.current();
    assert_eq!(
        stored.msek, msek_before_disable,
        "the shared MSEK is PRESERVED — CalDAV still needs it"
    );
    assert_eq!(
        stored.credentials.len(),
        1,
        "the read credential is PRESERVED — it AUTHs CalDAV"
    );
    assert!(!stored.is_mail_enabled(), "email is now disabled");
    assert!(stored.caldav_enabled, "CalDAV stays enabled");
    assert!(!machine.snapshot().enabled, "snapshot shows mail disabled");

    let s = nest.state();
    assert_eq!(
        s.revoke_submission_token.len(),
        1,
        "the outbound submission token was revoked (no more sending)"
    );
    assert!(
        s.revoke_wrapped_mls.is_empty(),
        "the read credential's wrapped-MSEK blob was NOT revoked (CalDAV AUTH)"
    );
    assert_eq!(
        s.set_mail_enabled,
        vec![true],
        "disable never flips the deployment-wide set_mail_enabled"
    );
}

#[tokio::test]
async fn disable_mail_on_caldav_only_user_returns_invalid_state() {
    // A genuinely CalDAV-only actor has no email to disable: `is_mail_enabled()`
    // is false, so DisableMail errors rather than clearing the MSEK their
    // calendar depends on. (In the UI the disable affordance isn't even shown —
    // the snapshot reports mail disabled.)
    let (machine, _nest, _mail) = build_machine();
    machine
        .enable_caldav_mailbox_with_generated_password("Default".into())
        .await
        .expect("enable caldav mailbox");
    let err = machine
        .dispatch(MailSettingsAction::DisableMail)
        .await
        .expect_err("a CalDAV-only actor has no mail to disable");
    assert!(format!("{err}").contains("not enabled"), "got: {err}");
}

#[tokio::test]
async fn enable_carddav_mailbox_mints_read_only_material_without_submission_or_toggle() {
    // CardDAV-only enablement (`carddav-server.md` § Independent enablement): the
    // contacts twin of the CalDAV-only test above — same shared MSEK (the
    // `bridge_carddav_*` store seals to the MSEK-derived recipient keypair), same
    // read-only recipe (no submission token), no deployment-toggle flip (that
    // Admin toggle is `BridgeApprovalMachine::SetCardDavEnabled`, fired
    // separately by the onboarding launch glue).
    let (machine, nest, mail) = build_machine();

    let pw = machine
        .enable_carddav_mailbox_with_generated_password("Default".into())
        .await
        .expect("enable carddav mailbox");

    {
        let s = nest.state();
        assert_eq!(s.provision_mls_snapshot.len(), 1, "snapshot blob");
        assert_eq!(s.provision_wrapped_mls.len(), 1, "wrapped-MSEK blob");
        assert_eq!(
            s.provision_recipient_pubkey.len(),
            1,
            "recipient pubkey registered"
        );
        assert_eq!(
            s.provision_submission_token.len(),
            0,
            "NO submission token — CardDAV needs no outbound mail (read-only recipe)"
        );
        assert!(
            s.set_mail_enabled.is_empty(),
            "NO deployment mail-toggle flip (owned by BridgeApprovalMachine::SetCardDavEnabled)"
        );
    }

    // CardDAV-only ⇒ snapshot reports mail DISABLED despite the shared MSEK,
    // and the per-actor carddav flag is recorded.
    let stored = mail.current();
    assert!(stored.msek.is_some(), "shared MSEK populated");
    assert!(stored.carddav_enabled, "CardDAV flag recorded");
    assert!(!stored.caldav_enabled, "CalDAV untouched");
    assert!(!stored.is_mail_enabled(), "email is not enabled");
    assert!(
        !machine.snapshot().enabled,
        "CardDAV-only ⇒ snapshot reports mail DISABLED despite the shared MSEK"
    );
    assert!(
        machine.snapshot().carddav_enabled,
        "snapshot surfaces carddav_enabled (the credential-section predicate reads it)"
    );
    let revealed = machine
        .reveal_credential_secret("default".to_string())
        .await
        .expect("reveal default");
    assert_eq!(
        revealed.as_str(),
        pw.as_str(),
        "revealed credential equals the returned password"
    );
}

#[tokio::test]
async fn enable_carddav_mailbox_is_idempotent_when_mailbox_already_provisioned() {
    // The MSEK is shared across email + CalDAV + CardDAV. If a mailbox already
    // exists, enabling CardDAV must NOT re-mint — just record the per-actor flag
    // (so a later "Disable mail" preserves the shared MSEK the address book
    // still needs).
    let (machine, nest, mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "Default".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(b"existing-mail-password-bytes".to_vec()),
        })
        .await
        .expect("enable mail");
    let msek_after_mail = mail.current().msek;
    let snapshots_after_mail = nest.state().provision_mls_snapshot.len();

    machine
        .enable_carddav_mailbox_with_generated_password("Contacts".into())
        .await
        .expect("enable carddav (idempotent on an existing mailbox)");

    assert_eq!(
        mail.current().msek,
        msek_after_mail,
        "shared MSEK preserved — CardDAV-enable did not re-mint"
    );
    assert_eq!(
        nest.state().provision_mls_snapshot.len(),
        snapshots_after_mail,
        "no new blob provisioning on the idempotent CardDAV-enable"
    );
    assert_eq!(
        mail.current().credentials.len(),
        1,
        "no extra credential added"
    );
    let stored = mail.current();
    assert!(stored.carddav_enabled, "CardDAV flag recorded");
    assert!(stored.is_mail_enabled(), "email stays enabled");
}

#[tokio::test]
async fn disable_mail_preserves_msek_and_read_credential_when_carddav_enabled() {
    // Disabling email on an actor who ALSO has CardDAV must NOT clear the shared
    // MSEK or revoke the read credential — the same preservation rule CalDAV
    // gets: the AEAD-unwrap blob also authenticates CardDAV and the address-book
    // store seals to the MSEK-derived recipient keypair. Only the outbound
    // submission tokens are revoked.
    let (machine, nest, mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "Default".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(b"shared-bridge-password-bytes".to_vec()),
        })
        .await
        .expect("enable mail");
    machine
        .enable_carddav_mailbox_with_generated_password("Contacts".into())
        .await
        .expect("enable carddav (idempotent — records the flag)");
    let msek_before_disable = mail.current().msek;

    machine
        .dispatch(MailSettingsAction::DisableMail)
        .await
        .expect("disable mail while CardDAV stays on");

    let stored = mail.current();
    assert_eq!(
        stored.msek, msek_before_disable,
        "the shared MSEK is PRESERVED — CardDAV still needs it"
    );
    assert_eq!(
        stored.credentials.len(),
        1,
        "the read credential is PRESERVED — it AUTHs CardDAV"
    );
    assert!(!stored.is_mail_enabled(), "email is now disabled");
    assert!(stored.carddav_enabled, "CardDAV stays enabled");

    let s = nest.state();
    assert_eq!(
        s.revoke_submission_token.len(),
        1,
        "the outbound submission token was revoked (no more sending)"
    );
    assert!(
        s.revoke_wrapped_mls.is_empty(),
        "the read credential's wrapped-MSEK blob was NOT revoked (CardDAV AUTH)"
    );
}

#[tokio::test]
async fn enable_mail_provisions_all_three_blobs() {
    let (machine, nest, mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "iPhone Mail".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(b"high-entropy-token-bytes".to_vec()),
        })
        .await
        .expect("enable mail");

    let s = nest.state();
    assert_eq!(s.provision_mls_snapshot.len(), 1, "snapshot blob");
    assert_eq!(s.provision_wrapped_mls.len(), 1, "wrapped-MSEK blob");
    assert_eq!(s.provision_submission_token.len(), 1, "submission token");
    // The recipient-mail pubkey is self-registered for this actor, and
    // it matches the MSEK-derived keypair.
    assert_eq!(
        s.provision_recipient_pubkey.len(),
        1,
        "recipient pubkey registered"
    );
    assert_eq!(s.provision_recipient_pubkey[0].0, ACTOR, "own actor");
    // Design A: enabling mail also flips the deployment-wide subsystem on, so a
    // real (Docker/systemd) deploy actually boots the mail bridge. The fake here
    // stands in for the admin (its set_mail_enabled succeeds).
    assert_eq!(
        s.set_mail_enabled,
        vec![true],
        "enable flips the subsystem on"
    );
    drop(s);

    let stored = mail.current();
    assert!(stored.msek.is_some(), "MSEK populated");
    let (_, expected_pk) =
        fauna_mls::wrapped_blob::derive_recipient_hpke_keypair(&stored.msek.unwrap());
    assert_eq!(
        nest.state().provision_recipient_pubkey[0].1,
        expected_pk,
        "registered pubkey is the MSEK-derived recipient key"
    );
    assert_eq!(stored.credentials.len(), 1);
    assert_eq!(stored.credentials[0].credential_id, "iphone-mail");
    assert_eq!(stored.credentials[0].display_name, "iPhone Mail");

    // Snapshot mirrors the persisted state.
    let snap = machine.snapshot();
    assert!(snap.enabled);
    assert_eq!(snap.credentials.len(), 1);
    assert_eq!(snap.credentials[0].credential_id, "iphone-mail");
    assert!(snap.error.is_none());
}

#[tokio::test]
async fn enable_mail_publishes_mlkem_ek_unconditionally() {
    // The client publishes the post-quantum ML-KEM ek alongside the X25519
    // recipient pubkey (S3d leg A) against a nest advertising NOTHING (the
    // default fake): no capability token gates it since the 2026-09-24 ruling
    // (`post-quantum.md` § Capability negotiation).
    let (machine, nest, mail) = build_machine();

    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "iPhone Mail".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(b"high-entropy-token-bytes".to_vec()),
        })
        .await
        .expect("enable mail");

    let msek = mail.current().msek.expect("MSEK populated");
    let expected_ek = fauna_mls::wrapped_blob::derive_recipient_xwing_keypair(&msek)
        .public
        .mlkem_encaps_key()
        .to_vec();

    let s = nest.state();
    assert_eq!(s.provision_recipient_pubkey.len(), 1);
    let published = &s.provision_recipient_pubkey[0].2;
    assert_eq!(published.len(), 1184, "ML-KEM-768 encaps-key length");
    assert_eq!(
        published, &expected_ek,
        "published ek is the MSEK-derived X-Wing ML-KEM half"
    );
}

#[tokio::test]
async fn enable_mail_publishes_epoch_keys_unconditionally() {
    // The client publishes the [e_now, e_now+HORIZON] per-epoch public halves
    // (content-sealing epochs, B3) alongside the standing X25519 recipient
    // pubkey, against a nest advertising NOTHING — the `mail-epoch-schedule`
    // token is retired (2026-09-24 ruling).
    let (machine, nest, mail) = build_machine();

    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "iPhone Mail".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(b"high-entropy-token-bytes".to_vec()),
        })
        .await
        .expect("enable mail");

    let msek = mail.current().msek.expect("MSEK populated");
    let s = nest.state();
    assert_eq!(s.provision_recipient_pubkey.len(), 1);
    let published = s.provision_recipient_pubkey[0]
        .3
        .as_ref()
        .expect("epoch schedule always published");

    assert_eq!(
        published.len(),
        (fauna_mls::wrapped_blob::MAIL_EPOCH_PUBLISH_HORIZON + 1) as usize,
        "publishes the full [e_now, e_now+HORIZON] horizon inclusive"
    );
    let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(
        fauna_core::data::Timestamp::now_secs() as u64,
    );
    assert_eq!(published[0].epoch, e_now, "first published epoch is e_now");
    let (_, want_pub) = fauna_mls::wrapped_blob::derive_recipient_epoch_hpke_keypair(&msek, e_now);
    assert_eq!(
        published[0].mls_pubkey.as_slice(),
        &want_pub,
        "epoch pubkey is the MSEK+epoch-derived X25519 half"
    );
}

#[tokio::test]
async fn enable_mail_publishes_both_epoch_halves_on_a_hybrid_recipient() {
    // INFO-B: a hybrid recipient must never get a
    // half-empty epoch publication — every entry carries both halves,
    // otherwise the recipient's new mail would silently downgrade to
    // classical-only at the epoch-sealing flip.
    let (machine, nest, mail) = build_machine();

    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "iPhone Mail".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(b"high-entropy-token-bytes".to_vec()),
        })
        .await
        .expect("enable mail");

    let msek = mail.current().msek.expect("MSEK populated");
    let s = nest.state();
    let published = s.provision_recipient_pubkey[0]
        .3
        .as_ref()
        .expect("epoch schedule published");
    assert!(
        !published.is_empty(),
        "the horizon must be non-empty for this assertion to mean anything"
    );
    assert!(
        published.iter().all(|k| k.mlkem_ek.len() == 1184),
        "every epoch entry carries both halves"
    );
    let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(
        fauna_core::data::Timestamp::now_secs() as u64,
    );
    let want_ek = fauna_mls::wrapped_blob::derive_recipient_epoch_xwing_keypair(&msek, e_now)
        .public
        .mlkem_encaps_key()
        .to_vec();
    assert_eq!(
        published[0].mlkem_ek.as_slice(),
        &want_ek,
        "epoch ML-KEM half is the MSEK+epoch-derived X-Wing encaps key"
    );
}

#[tokio::test]
async fn refresh_epoch_schedule_is_a_noop_before_mail_is_enabled() {
    let (machine, nest, _mail) = build_machine();

    machine
        .refresh_epoch_schedule()
        .await
        .expect("no-op before mail is enabled");

    assert!(
        nest.state().provision_recipient_pubkey.is_empty(),
        "no MSEK yet — nothing to publish"
    );
}

#[tokio::test]
async fn refresh_epoch_schedule_republishes_the_horizon_on_connect() {
    // Design 2026-07-18 § 3: "refreshes the horizon on every connect" —
    // every app calls this once per successful (re)connect so the
    // published horizon keeps sliding forward.
    let (machine, nest, mail) = build_machine();

    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "Default".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(b"round-trip-plain-pw-24chars!".to_vec()),
        })
        .await
        .expect("enable mail");
    let msek = mail.current().msek.expect("MSEK populated");

    machine
        .refresh_epoch_schedule()
        .await
        .expect("connect-time refresh");

    let s = nest.state();
    // enable_mail's own publish (1) + the explicit refresh (1).
    assert_eq!(s.provision_recipient_pubkey.len(), 2);
    let refreshed = s.provision_recipient_pubkey[1]
        .3
        .as_ref()
        .expect("refresh republishes the schedule");
    let e_now = fauna_mls::wrapped_blob::mail_sealing_epoch_of(
        fauna_core::data::Timestamp::now_secs() as u64,
    );
    let (_, want_pub) = fauna_mls::wrapped_blob::derive_recipient_epoch_hpke_keypair(&msek, e_now);
    assert_eq!(refreshed[0].mls_pubkey.as_slice(), &want_pub);
}

#[tokio::test]
async fn a_connect_refresh_that_publishes_an_ek_also_rewrites_the_snapshot() {
    // The paired-publication invariant, stated on `LeafInitKeypair::mlkem_dk`
    // (`fauna-mls/src/wrapped_blob/mls_snapshot_plaintext.rs`): an absent `mdk`
    // is safe "since hybrid mail is only sealed once the recipient has
    // *published* the matching ek …, which the same client transaction that
    // (re)writes this snapshot performs."
    //
    // The two halves of the X-Wing recipient keypair reach the nest by
    // different routes: the **ek** (public, what the MDA seals to) rides
    // `provision_recipient_mls_pubkey`, while the **dk** (private, what the MDA
    // opens with) rides only inside the snapshot blob. `refresh_epoch_schedule`
    // runs on EVERY reconnect and publishes the ek — so if it does not also
    // rewrite the snapshot (by construction, before the pubkey), the actor starts advertising a
    // decapsulation key the MDA does not hold. Every CalDAV/CardDAV collection
    // the MDA then seals is unopenable by the very session that sealed it,
    // surfacing as `HPKE open failed: no matching leaf keypair in snapshot` and
    // a calendar silently dropped from the PROPFIND home set.
    let (machine, nest, _mail) = build_machine();

    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "Default".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(b"round-trip-plain-pw-24chars!".to_vec()),
        })
        .await
        .expect("enable mail");

    let snapshots_after_enable = nest.state().provision_mls_snapshot.len();

    machine
        .refresh_epoch_schedule()
        .await
        .expect("connect-time refresh");

    let s = nest.state();
    let last = s
        .provision_recipient_pubkey
        .last()
        .expect("the refresh published a recipient pubkey");
    assert_eq!(
        last.2.len(),
        1184,
        "precondition: the refresh publishes an ek — \
         without one this test asserts nothing",
    );
    assert!(
        s.provision_mls_snapshot.len() > snapshots_after_enable,
        "a refresh that publishes the ML-KEM ek must rewrite the snapshot in the \
         same transaction, so the matching decapsulation key is on the nest \
         before anything can seal to the ek ({} snapshot writes before, {} after)",
        snapshots_after_enable,
        s.provision_mls_snapshot.len(),
    );
}

#[tokio::test]
async fn add_credential_provisions_two_blobs_and_reuses_msek() {
    let (machine, nest, mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAB; 32]),
        })
        .await
        .expect("enable");
    let msek_after_enable = mail.current().msek;
    machine
        .dispatch(MailSettingsAction::AddCredential {
            display_name: "Thunderbird".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xCD; 32]),
        })
        .await
        .expect("add credential");

    let s = nest.state();
    // 1 from enable + 1 from add.
    assert_eq!(s.provision_mls_snapshot.len(), 1, "snapshot only at enable");
    assert_eq!(s.provision_wrapped_mls.len(), 2);
    assert_eq!(s.provision_submission_token.len(), 2);
    drop(s);

    let stored = mail.current();
    assert_eq!(stored.msek, msek_after_enable, "MSEK preserved");
    assert_eq!(stored.credentials.len(), 2);
    let ids: Vec<&str> = stored
        .credentials
        .iter()
        .map(|c| c.credential_id.as_str())
        .collect();
    assert_eq!(ids, vec!["default", "thunderbird"]);
}

#[tokio::test]
async fn add_credential_without_enable_returns_invalid_state() {
    let (machine, _nest, _mail) = build_machine();
    let err = machine
        .dispatch(MailSettingsAction::AddCredential {
            display_name: "x".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(b"pw".to_vec()),
        })
        .await
        .expect_err("must reject");
    let msg = format!("{err}");
    assert!(msg.contains("mail not enabled"), "got: {msg}");
}

#[tokio::test]
async fn enable_mail_twice_returns_invalid_state() {
    let (machine, _nest, _mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "x".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(b"tok".to_vec()),
        })
        .await
        .expect("first enable");
    let err = machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "y".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(b"tok2".to_vec()),
        })
        .await
        .expect_err("second enable must fail");
    let msg = format!("{err}");
    assert!(msg.contains("already enabled"), "got: {msg}");
}

#[tokio::test]
async fn enable_mail_swallows_set_mail_enabled_rejection_for_non_admin() {
    // Design A scoping: set_mail_enabled is Admin-class on the nest, so a
    // non-admin actor enabling their own mailbox gets a `Rejected` — which is a
    // no-op, not a failure. The user's mailbox must still come up.
    let (machine, nest, mail) = build_machine();
    nest.state().fail_set_mail_enabled_with = Some(
        fauna_client_mail_settings::NestError::Rejected("forbidden: admin only".into()),
    );
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAB; 32]),
        })
        .await
        .expect("non-admin enable still succeeds (subsystem flip is a no-op)");
    // The mailbox was fully provisioned + persisted despite the swallowed reject.
    assert!(mail.current().msek.is_some(), "MSEK populated");
    assert!(machine.snapshot().enabled);
}

#[tokio::test]
async fn enable_mail_surfaces_transient_set_mail_enabled_failure() {
    // A `Transient` (connectivity) failure on the subsystem flip rides the same
    // WS that just provisioned the mailbox, so it surfaces — the user learns the
    // enable didn't fully land rather than silently getting a non-serving deploy.
    let (machine, nest, _mail) = build_machine();
    nest.state().fail_set_mail_enabled_with = Some(
        fauna_client_mail_settings::NestError::Transient("nest unreachable".into()),
    );
    let err = machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAB; 32]),
        })
        .await
        .expect_err("transient subsystem-flip failure surfaces");
    assert!(format!("{err}").contains("unreachable"), "got: {err}");
}

#[tokio::test]
async fn revoke_credential_drops_row_and_calls_two_revokes() {
    let (machine, nest, mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAB; 32]),
        })
        .await
        .expect("enable");
    machine
        .dispatch(MailSettingsAction::AddCredential {
            display_name: "iPhone".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xCD; 32]),
        })
        .await
        .expect("add");
    machine
        .dispatch(MailSettingsAction::RevokeCredential {
            credential_id: "iphone".into(),
        })
        .await
        .expect("revoke");

    let s = nest.state();
    assert_eq!(s.revoke_wrapped_mls.len(), 1);
    assert_eq!(s.revoke_submission_token.len(), 1);
    assert_eq!(s.revoke_wrapped_mls[0], (ACTOR, "iphone".to_string()));
    drop(s);

    let stored = mail.current();
    assert_eq!(stored.credentials.len(), 1);
    assert_eq!(stored.credentials[0].credential_id, "default");
}

#[tokio::test]
async fn revoke_unknown_credential_returns_error() {
    let (machine, _nest, _mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAB; 32]),
        })
        .await
        .expect("enable");
    let err = machine
        .dispatch(MailSettingsAction::RevokeCredential {
            credential_id: "no-such-credential".into(),
        })
        .await
        .expect_err("must reject");
    let msg = format!("{err}");
    assert!(msg.contains("unknown credential_id"), "got: {msg}");
}

/// The email-only teardown revokes every credential and turns email off, and
/// the MSEK outlives it — dormant, since the mail custody's state row keeps it
/// present-wins and no write can clear it (`mail-credentials.md`, the
/// Disable-mail row). A re-enable mints a fresh credential under the SAME key,
/// under an id no revoked row has held, so mail sealed before the disable stays
/// openable.
#[tokio::test]
async fn disable_mail_revokes_every_credential_and_a_re_enable_reuses_the_dormant_msek() {
    let (machine, nest, mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAB; 32]),
        })
        .await
        .expect("enable");
    machine
        .dispatch(MailSettingsAction::AddCredential {
            display_name: "iPhone".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xCD; 32]),
        })
        .await
        .expect("add");
    let msek_before = mail.current().msek.expect("MSEK minted");

    machine
        .dispatch(MailSettingsAction::DisableMail)
        .await
        .expect("disable");

    {
        let s = nest.state();
        // One revoke (MLS blob + submission token) per credential — the
        // goal-doc "one RevokeCredential per row" sequence.
        assert_eq!(s.revoke_wrapped_mls.len(), 2, "two MLS-blob revokes");
        assert_eq!(s.revoke_submission_token.len(), 2, "two token revokes");
        // Disable must NOT touch the Admin-scoped deployment-wide subsystem
        // toggle: only the single `true` from enable should be recorded.
        assert_eq!(
            s.set_mail_enabled,
            vec![true],
            "disable must not flip the deployment-wide set_mail_enabled"
        );
    }

    // Email is off, every credential is revoked (hidden by the fold, its id
    // spent), and the MSEK stays — dormant.
    let stored = mail.current();
    assert!(!stored.is_mail_enabled(), "email off");
    assert!(stored.credentials.is_empty(), "every credential revoked");
    assert_eq!(
        stored.msek,
        Some(msek_before.clone()),
        "the MSEK is dormant, not cleared"
    );
    let rows = mail.rows();
    assert_eq!(rows.spent_credential_ids(), ["default", "iphone"]);
    assert!(
        rows.credentials
            .values()
            .all(|c| c.revoked_at_unix.is_some()
                && c.secret.is_empty()
                && c.wrapped_under.is_none()),
        "each revoked row carries the marker, no secret and no generation"
    );

    // Snapshot reflects disabled.
    let snap = machine.snapshot();
    assert!(!snap.enabled, "snapshot disabled");
    assert!(snap.credentials.is_empty());
    assert!(snap.error.is_none());

    // Re-enable reuses the dormant MSEK and mints a fresh credential under an
    // unspent id.
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xEF; 32]),
        })
        .await
        .expect("re-enable");
    assert!(machine.snapshot().enabled, "re-enabled");
    let stored = mail.current();
    assert_eq!(stored.msek, Some(msek_before), "the same MSEK");
    let ids: Vec<&str> = stored
        .credentials
        .iter()
        .map(|c| c.credential_id.as_str())
        .collect();
    assert_eq!(ids.len(), 1, "one live credential: {ids:?}");
    assert_ne!(ids[0], "default", "a revoked id is spent, never re-minted");
    assert_eq!(
        stored.credentials[0].wrapped_under,
        stored
            .msek
            .as_ref()
            .map(fauna_core::data::MsekFingerprint::of),
        "the fresh row names the generation it is wrapped under"
    );
}

#[tokio::test]
async fn disable_mail_when_not_enabled_returns_invalid_state() {
    let (machine, _nest, _mail) = build_machine();
    let err = machine
        .dispatch(MailSettingsAction::DisableMail)
        .await
        .expect_err("must reject when mail is not enabled");
    let msg = format!("{err}");
    assert!(msg.contains("not enabled"), "got: {msg}");
}

#[tokio::test]
async fn default_snapshot_serving_enabled_is_on() {
    // The per-actor IMAP/CalDAV-serving flag defaults ON (absent ⇒ on), so a
    // freshly built machine — before any hydrate — already reads serving on.
    let (machine, _nest, _mail) = build_machine();
    assert!(
        machine.snapshot().serving_enabled,
        "serving defaults on (deployment-home-with-public-relay.md § MUA reach)"
    );
}

#[tokio::test]
async fn set_serving_enabled_records_nest_call_and_flips_snapshot() {
    // Flipping the serving toggle fires the caller-scoped
    // `set_mail_serving_enabled` RPC and reflects in the snapshot. No MailConfig
    // change (the flag lives on the nest), so no credential/blob side effects.
    let (machine, nest, mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::SetServingEnabled { enabled: false })
        .await
        .expect("set serving off");
    assert_eq!(nest.state().set_mail_serving_enabled, vec![false]);
    assert!(!machine.snapshot().serving_enabled, "snapshot reflects off");
    assert!(machine.snapshot().error.is_none());
    // The synced mail custody is untouched (the flag is a nest-side per-actor row).
    assert!(mail.current().msek.is_none(), "no mail enable side effect");

    machine
        .dispatch(MailSettingsAction::SetServingEnabled { enabled: true })
        .await
        .expect("set serving on");
    assert_eq!(nest.state().set_mail_serving_enabled, vec![false, true]);
    assert!(machine.snapshot().serving_enabled, "snapshot reflects on");
}

#[tokio::test]
async fn hydrate_reads_serving_flag_from_nest() {
    // hydrate() refreshes serving_enabled from the nest (a caller-scoped read),
    // separately from the synced mail custody — a nest that reports the actor
    // turned serving off lands as serving_enabled=false even though the default
    // is on.
    let (machine, nest, _mail) = build_machine();
    nest.state().mail_serving_enabled = Some(false);
    machine.hydrate().await.expect("hydrate");
    assert!(
        !machine.snapshot().serving_enabled,
        "hydrate read the nest's false"
    );

    // A never-set actor (None) hydrates back to the default-on.
    nest.state().mail_serving_enabled = None;
    machine.hydrate().await.expect("hydrate again");
    assert!(machine.snapshot().serving_enabled, "default-on round-trips");
}

fn build_machine_for(node_url: &str) -> (MailSettingsMachine, FakeNestClient) {
    let nest = FakeNestClient::new();
    let cfg = FakeSuccessionLedgerStore::empty(fauna_core::identity::ActorId(ACTOR));
    let machine = MailSettingsMachine::new(
        ACTOR,
        Arc::new(nest.clone()),
        Arc::new(cfg),
        Arc::new(FakeMailStore::empty()),
        Arc::new(FakeSigner::new(SIGNER_SEED)),
        MuaInstructions::for_node_url(node_url),
    );
    (machine, nest)
}

#[tokio::test]
async fn hydrate_threads_admin_caldav_port_for_local_target_only() {
    // Local-target box (bare IP): the MDA binds the admin port directly (no SNI
    // router), so hydrate threads the synced `get_caldav_port` value into the
    // displayed CalDAV connection-detail port.
    let (local, nest) = build_machine_for("https://192.168.1.50:8443");
    nest.state().caldav_port = Some(9000);
    local.hydrate().await.expect("hydrate local");
    assert_eq!(
        local.snapshot().mua.caldav_port,
        9000,
        "local-target box shows the admin-set port"
    );
    // It survives a config-driven re-snapshot (set_snapshot_from carries it).
    local
        .dispatch(MailSettingsAction::SetServingEnabled { enabled: false })
        .await
        .expect("serving off");
    assert_eq!(
        local.snapshot().mua.caldav_port,
        9000,
        "admin port preserved across a dispatch re-snapshot"
    );

    // Registrable-domain box: CalDAV is router-fronted at the public 443, so the
    // admin singleton is ignored even though the nest reports 9000.
    let (domain, nest2) = build_machine_for("https://nest.example.com:8443");
    nest2.state().caldav_port = Some(9000);
    domain.hydrate().await.expect("hydrate domain");
    assert_eq!(
        domain.snapshot().mua.caldav_port,
        443,
        "registrable domain stays on the router-fronted 443"
    );
}

#[tokio::test]
async fn hydrate_degrades_to_default_when_get_caldav_port_fails() {
    // Best-effort read: a failed `get_caldav_port` (here a rejection) is
    // ignored. The page must still hydrate, with the
    // local-target port falling back to the for-node-URL default
    // (DEFAULT_CALDAV_PORT) rather than failing the whole hydrate.
    let (local, nest) = build_machine_for("https://10.1.8.51");
    nest.state().fail_get_caldav_port_with = Some(NestError::Rejected("unknown kind".into()));
    local.hydrate().await.expect("hydrate still succeeds");
    assert_eq!(
        local.snapshot().mua.caldav_port,
        fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT,
        "a rejected read degrades to the for-node-URL default"
    );
}

#[tokio::test]
async fn serving_a_webdav_set_alone_makes_credential_management_reachable() {
    // `webdav-server.md` § Independent enablement pt 1: the credential-management
    // reachability predicate extends to the actor's WebDAV state. An actor with
    // email + CalDAV + CardDAV all OFF, but serving one folder over WebDAV, must
    // still reach the credential section — its WebDAV client AUTHs under the same
    // `default` bridge credential, so hiding the section would strand it.
    let (machine, nest, _mail) = build_machine();
    nest.state().serves_any_webdav_set = true;
    machine.hydrate().await.expect("hydrate");
    let snap = machine.snapshot();
    assert!(!snap.enabled && !snap.caldav_enabled && !snap.carddav_enabled);
    assert!(snap.serves_webdav_set, "the nest fold reached the snapshot");
    assert!(
        snap.credential_management_reachable,
        "serving a WebDAV set alone must open the credential section"
    );
}

#[tokio::test]
async fn credential_management_is_unreachable_for_a_bare_actor() {
    // The other half of the predicate: nothing enabled, nothing served ⇒ no
    // credential section (an actor with no mailbox has no credential to manage).
    // This is what the CalDAV-only gating exists to preserve, and why the
    // predicate reads the *per-actor* serve state and not the deployment-wide
    // `webdav_enabled` toggle (which defaults ON for a real-domain box).
    let (machine, _nest, _mail) = build_machine();
    machine.hydrate().await.expect("hydrate");
    let snap = machine.snapshot();
    assert!(!snap.serves_webdav_set);
    assert!(!snap.credential_management_reachable);
}

#[tokio::test]
async fn hydrate_degrades_to_not_serving_when_the_webdav_read_fails() {
    // Best-effort read: a rejected per-actor WebDAV read is ignored. The page must still hydrate, with
    // the WebDAV URL row simply hidden — never a failed hydrate.
    let (machine, nest, _mail) = build_machine();
    nest.state().fail_serves_any_webdav_set_with = Some(NestError::Rejected("unknown kind".into()));
    machine.hydrate().await.expect("hydrate still succeeds");
    assert!(!machine.snapshot().serves_webdav_set);
}

#[tokio::test]
async fn serves_webdav_set_survives_a_credential_dispatch() {
    // `serves_webdav_set` isn't part of MailConfig, so `set_snapshot_from` (run on
    // every enable/add/revoke/rotate) must carry it forward — and the predicate it
    // feeds must stay true — rather than reset to the default. The exact trap
    // `serving_enabled` already guards against.
    let (machine, nest, _mail) = build_machine();
    nest.state().serves_any_webdav_set = true;
    machine.hydrate().await.expect("hydrate");
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAB; 32]),
        })
        .await
        .expect("enable");
    let snap = machine.snapshot();
    assert!(
        snap.serves_webdav_set,
        "serve state preserved across the enable re-snapshot"
    );
    assert!(snap.credential_management_reachable);
}

#[tokio::test]
async fn serving_flag_survives_a_credential_dispatch() {
    // serving_enabled isn't part of MailConfig, so set_snapshot_from (run on
    // every enable/add/revoke/rotate) must carry it forward rather than reset to
    // the default. Turn it off, then enable mail, and it stays off.
    let (machine, _nest, _mail) = build_machine();
    machine
        .dispatch(MailSettingsAction::SetServingEnabled { enabled: false })
        .await
        .expect("serving off");
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(vec![0xAB; 32]),
        })
        .await
        .expect("enable");
    assert!(machine.snapshot().enabled, "mail enabled");
    assert!(
        !machine.snapshot().serving_enabled,
        "serving flag preserved across the enable re-snapshot"
    );
}

#[tokio::test]
async fn submission_token_is_signed_by_identity_seed() {
    use fauna_mls::wrapped_blob::unseal_submission_token;
    let nest = FakeNestClient::new();
    let signer = FakeSigner::new(SIGNER_SEED);
    let vk = signer.verifying_key();
    let machine = MailSettingsMachine::new(
        ACTOR,
        Arc::new(nest.clone()),
        Arc::new(FakeSuccessionLedgerStore::empty(
            fauna_core::identity::ActorId(ACTOR),
        )),
        Arc::new(FakeMailStore::empty()),
        Arc::new(signer),
        MuaInstructions::placeholder(),
    );
    // OAUTHBEARER → HKDF (fast); 32-byte random token bytes.
    let token_secret = vec![0xDE; 32];
    machine
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "default".into(),
            kind: CredentialKind::OAuthBearer,
            secret: SecretBytes::from(token_secret.clone()),
        })
        .await
        .expect("enable");
    let s = nest.state();
    let token_blob = s.provision_submission_token[0].clone();
    drop(s);
    // Unseal with the same credential bytes + the signer's pubkey.
    use fauna_mls::wrapped_blob::CredentialInput;
    let unwrapped = unseal_submission_token(
        &token_blob,
        &CredentialInput::OauthBearer(&token_secret),
        &vk,
    )
    .expect("unseal");
    assert_eq!(unwrapped.actor_id, ACTOR.to_vec());
    assert_eq!(unwrapped.credential_id, "default");
}

/// Home-with-public-relay: the user enables mail on the public relay box, then
/// the client provisions the **same** mailbox onto the private home box reusing
/// the fleet MSEK (the GUI-shaped flow — no seal-helper, no `provision_relay_user`
/// script). Proves the load-bearing property: both boxes register the SAME
/// MSEK-derived recipient pubkey, so mail the relay seals to the recipient key is
/// decryptable by the home box's MDA. The home box gets the read recipe only (no
/// submission token — it runs no MTA).
/// Goal: `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
/// § Inbound mail; `docs/goal/behavior/mail-credentials.md` § MSEK lifecycle.
#[tokio::test]
async fn provision_relay_mailbox_reuses_fleet_msek_across_paired_boxes() {
    use fauna_mls::wrapped_blob::{
        CredentialInput, derive_recipient_hpke_keypair, unseal_wrapped_msek,
    };

    // One account mail custody (`fauna.state.mail`), shared by the two machines
    // a multi-homed client binds — one to the public relay box, one to the
    // private home box. `FakeMailStore` shares its rows across clones (the
    // account plane every device of the account reads, whichever box it is
    // connected to), so the home-box machine reads the MSEK the public-box
    // machine minted — exactly what a real peer-bound machine does.
    let cfg = FakeSuccessionLedgerStore::empty(fauna_core::identity::ActorId(ACTOR));
    let mail = FakeMailStore::empty();
    let pw = b"relay-read-pw-24-chars-long!".to_vec();

    // Box A — public relay box: the user enables mail here (mints the MSEK).
    let nest_a = FakeNestClient::new();
    let machine_a = MailSettingsMachine::new(
        ACTOR,
        Arc::new(nest_a.clone()),
        Arc::new(cfg.clone()),
        Arc::new(mail.clone()),
        Arc::new(FakeSigner::new(SIGNER_SEED)),
        MuaInstructions::placeholder(),
    );
    // Box B — private home box: provisioned by reusing the fleet MSEK (no enable,
    // no fresh MSEK).
    let nest_b = FakeNestClient::new();
    let machine_b = MailSettingsMachine::new(
        ACTOR,
        Arc::new(nest_b.clone()),
        Arc::new(cfg.clone()),
        Arc::new(mail.clone()),
        Arc::new(FakeSigner::new(SIGNER_SEED)),
        MuaInstructions::placeholder(),
    );

    machine_a
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "Default".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(pw.clone()),
        })
        .await
        .expect("enable mail on the public box");

    machine_b
        .dispatch(MailSettingsAction::ProvisionRelayMailbox)
        .await
        .expect("provision the mailbox on the home box");

    let a = nest_a.state();
    let b = nest_b.state();

    // Load-bearing: BOTH boxes register the SAME MSEK-derived recipient pubkey.
    // A *different* MSEK on box B would silently break home-box decrypt.
    assert_eq!(
        a.provision_recipient_pubkey[0].1, b.provision_recipient_pubkey[0].1,
        "both boxes register the same MSEK-derived recipient pubkey"
    );
    let fleet_msek = mail.current().msek.expect("fleet MSEK persisted by enable");
    let (_, want_pub) = derive_recipient_hpke_keypair(&fleet_msek);
    assert_eq!(
        b.provision_recipient_pubkey[0].1, want_pub,
        "the home box's recipient pubkey is derived from the fleet MSEK"
    );

    // Box B got the full READ recipe and NO submission token.
    assert_eq!(
        b.provision_mls_snapshot.len(),
        1,
        "MLS snapshot on home box"
    );
    assert_eq!(
        b.provision_recipient_pubkey.len(),
        1,
        "recipient pubkey on home box"
    );
    assert_eq!(
        b.provision_wrapped_mls.len(),
        1,
        "wrapped-MSEK read credential on home box"
    );
    assert!(
        b.provision_submission_token.is_empty(),
        "no submission token on the home box — it never submits"
    );
    // The home box's wrapped-MSEK opens with the user's password to exactly the
    // fleet MSEK — the read credential the MDA AEAD-unwraps at MUA-AUTH.
    let unwrapped = unseal_wrapped_msek(&b.provision_wrapped_mls[0], &CredentialInput::Plain(&pw))
        .expect("home-box wrapped-MSEK unseals with the user's password");
    assert_eq!(
        *unwrapped, fleet_msek,
        "home-box read credential unwraps to the fleet MSEK"
    );

    // The relay provision writes nothing to the mail custody (it already holds
    // the MSEK + credential; this only pushes non-federating bridge blobs).
    assert_eq!(
        mail.current().credentials.len(),
        1,
        "no spurious credential added by the relay provision"
    );
}

/// Home-with-public-relay, **plaintext home box** — Phase-3 D3
/// (`2026-07-07-phase-3-sealed-both-modes-design.md`): the plaintext-MSEK
/// deposit leg is RETIRED. Even on a home box committed to plaintext storage
/// mode, the relay provision lands the full read recipe but **never transmits
/// the plaintext fleet MSEK** — relayed mail stays sealed at rest and is opened
/// at the AUTH'd MDA session / the client. (The `FakeNestClient` no longer even
/// models the deposit; this test pins that the provision succeeds without it.)
#[tokio::test]
async fn provision_relay_mailbox_never_deposits_plaintext_msek() {
    let cfg = FakeSuccessionLedgerStore::empty(fauna_core::identity::ActorId(ACTOR));
    let mail = FakeMailStore::empty();
    let pw = b"relay-read-pw-24-chars-long!".to_vec();

    // Box A — public relay box: the user enables mail here (mints the MSEK).
    let nest_a = FakeNestClient::new();
    let machine_a = MailSettingsMachine::new(
        ACTOR,
        Arc::new(nest_a.clone()),
        Arc::new(cfg.clone()),
        Arc::new(mail.clone()),
        Arc::new(FakeSigner::new(SIGNER_SEED)),
        MuaInstructions::placeholder(),
    );
    // Box B — the private home box (once a plaintext-mode one, so the retired
    // deposit fired; every box is sealed now, and the mode knob is gone).
    let nest_b = FakeNestClient::new();
    let machine_b = MailSettingsMachine::new(
        ACTOR,
        Arc::new(nest_b.clone()),
        Arc::new(cfg.clone()),
        Arc::new(mail.clone()),
        Arc::new(FakeSigner::new(SIGNER_SEED)),
        MuaInstructions::placeholder(),
    );

    machine_a
        .dispatch(MailSettingsAction::EnableMail {
            display_name: "Default".into(),
            kind: CredentialKind::Plain,
            secret: SecretBytes::from(pw.clone()),
        })
        .await
        .expect("enable mail on the public box");
    machine_b
        .dispatch(MailSettingsAction::ProvisionRelayMailbox)
        .await
        .expect("provision the mailbox on the plaintext home box");

    let _fleet_msek = mail.current().msek.expect("fleet MSEK persisted by enable");
    let b = nest_b.state();
    // The full read recipe landed; no plaintext-MSEK deposit exists anymore
    // (Phase-3 D3 — the FakeNestClient has no deposit surface to record).
    assert_eq!(
        b.provision_mls_snapshot.len(),
        1,
        "MLS snapshot on home box"
    );
    assert_eq!(
        b.provision_wrapped_mls.len(),
        1,
        "wrapped-MSEK read credential on home box"
    );
    assert!(
        b.provision_submission_token.is_empty(),
        "no submission token on the home box — it never submits"
    );
}

/// Provisioning a relay mailbox before mail is enabled (no fleet MSEK to reuse)
/// must error rather than mint a fresh one — minting here is the very bug a
/// second box would hit.
#[tokio::test]
async fn provision_relay_mailbox_without_enabled_mail_errors() {
    let (machine, nest, _mail) = build_machine();
    let err = machine
        .dispatch(MailSettingsAction::ProvisionRelayMailbox)
        .await
        .expect_err("provisioning a relay mailbox before mail is enabled must error");
    assert!(matches!(err, DispatchError::InvalidState(_)), "got {err:?}");
    assert!(
        nest.state().provision_mls_snapshot.is_empty(),
        "nothing provisioned when there is no MSEK to reuse"
    );
}

// ─── auto_enable_mail_for_new_user: the gated first-setup new-user auto-mint ───
// `mail-credentials.md` § Auto-enable for new users. Unlike the bare
// `enable_mail_with_generated_password`, this applies the deployment-policy
// gate (email_enabled && auto_enable_mail_for_new_users) and the
// already-has-a-mailbox gate, returning `Ok(None)` (not an error) when a gate
// fails — so the per-app first-setup launch glue can call it unconditionally.

#[tokio::test]
async fn auto_enable_mail_for_new_user_mints_when_policy_on_and_no_mailbox() {
    let (machine, _nest, mail) = build_machine();

    // Simulate a non-admin: the Admin-class set_mail_enabled is a no-op for them
    // (swallowed Rejected), exactly the new-user case this method targets.
    _nest.state().fail_set_mail_enabled_with = Some(
        fauna_client_mail_settings::NestError::Rejected("forbidden: admin only".into()),
    );

    let pw = machine
        .auto_enable_mail_for_new_user(
            /* deployment_mail_enabled */ true,
            /* policy */ true,
            "Default".into(),
        )
        .await
        .expect("auto-enable runs")
        .expect("a mailbox is minted when both flags are on and none exists");

    assert!(machine.snapshot().enabled, "mail enabled after auto-mint");
    assert!(mail.current().msek.is_some(), "MSEK populated");

    // The returned password is the one sealed into the credential (revealable
    // once for the user) — credential_id "Default" → "default".
    let revealed = machine
        .reveal_credential_secret("default".to_string())
        .await
        .expect("reveal default");
    assert_eq!(
        revealed.as_str(),
        pw.as_str(),
        "returned password is the sealed one"
    );
}

#[tokio::test]
async fn auto_enable_mail_for_new_user_skips_when_deployment_mail_off() {
    let (machine, _nest, mail) = build_machine();
    let minted = machine
        .auto_enable_mail_for_new_user(
            /* deployment_mail_enabled */ false,
            /* policy */ true,
            "Default".into(),
        )
        .await
        .expect("auto-enable runs");
    assert!(minted.is_none(), "no mint when the deployment has mail off");
    assert!(mail.current().msek.is_none(), "no MSEK minted");
    assert!(!machine.snapshot().enabled);
}

#[tokio::test]
async fn auto_enable_mail_for_new_user_skips_when_policy_off() {
    let (machine, _nest, mail) = build_machine();
    let minted = machine
        .auto_enable_mail_for_new_user(
            /* deployment_mail_enabled */ true,
            /* policy */ false,
            "Default".into(),
        )
        .await
        .expect("auto-enable runs");
    assert!(
        minted.is_none(),
        "no mint when the auto-enable policy is off"
    );
    assert!(mail.current().msek.is_none(), "no MSEK minted");
}

#[tokio::test]
async fn auto_enable_mail_for_new_user_is_noop_when_mailbox_already_exists() {
    let (machine, _nest, _mail) = build_machine();
    // First call mints.
    machine
        .auto_enable_mail_for_new_user(true, true, "Default".into())
        .await
        .expect("auto-enable runs")
        .expect("first call mints");
    // Second call (e.g. a re-trigger) must NOT re-mint and must NOT error — it
    // returns Ok(None), distinguishing it from the bare helper's InvalidState.
    let again = machine
        .auto_enable_mail_for_new_user(true, true, "Default".into())
        .await
        .expect("second auto-enable is a graceful no-op, not an error");
    assert!(again.is_none(), "no second mailbox is minted");
}
