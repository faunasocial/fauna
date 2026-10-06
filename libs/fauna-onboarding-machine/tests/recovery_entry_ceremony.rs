//! `recovery-entry-submit-button` — the phrase-only identity restore.
//!
//! Per `docs/goal/behavior/onboarding.md` § 1 Identity (page `recovery_entry`,
//! ratified 2026-08-01) and `identity-succession.md` § Seed escrow → *Restore
//! path*. These drive `OnboardingMachine::submit_recovery_entry` over
//! `FakeNestApi` — the transport-agnostic seam — so they pin the machine's own
//! decisions: which account the ceremony targets, what reaches the wire, where
//! each refusal leaves the user, and that a success lands exactly where an
//! import lands. The wire ceremony itself (challenge → sign → fetch → unseal)
//! is proven in `libs/fauna-client-recovery/tests/ceremonies.rs` against a
//! recording transport, and end to end in the tier_3 journey.

use std::sync::Arc;

use fauna_onboarding_machine::nest_api::{
    FakeNestApi, NestApi, RestoreSeedError, RestoredIdentity, RestoredPredecessorSeed,
};
use fauna_onboarding_machine::observer::NullObserver;
use fauna_onboarding_machine::{
    OnboardingMachine, OnboardingObserver, OnboardingStep, RecoveryEntryOutcome,
};

fn machine_with_fake() -> (Arc<OnboardingMachine>, Arc<FakeNestApi>) {
    let observer: Arc<dyn OnboardingObserver> = Arc::new(NullObserver);
    let fake = Arc::new(FakeNestApi::new());
    let m = OnboardingMachine::with_nest_api(observer, fake.clone() as Arc<dyn NestApi>);
    m.begin_recovery_entry();
    (m, fake)
}

fn secret_hex() -> String {
    "ab".repeat(32)
}

fn actor_hex() -> String {
    "cd".repeat(32)
}

fn restored_seed_hex() -> String {
    "ef".repeat(32)
}

/// A kit URI carrying both account halves — what the Settings ceremony mints.
fn full_kit_uri() -> String {
    fauna_core::recovery::RecoveryKitQr::to_uri(
        &secret_hex(),
        Some(&actor_hex()),
        Some("alice@fauna.test"),
    )
}

// ── The happy path ──────────────────────────────────────────────────────────

/// A Settings-minted kit restores with nothing typed: its `handle=` locates the
/// nest and its `actor=` names the account, so the account field stays empty.
/// The landing is `handle_entry` with the seed held and the handle carried —
/// deliberately identical to `confirm_imported_identity`, because the seed *is*
/// recovered and everything downstream is an import.
#[tokio::test]
async fn a_kit_carrying_its_handle_restores_with_nothing_typed() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());

    let outcome = m.submit_recovery_entry(full_kit_uri()).await;

    assert_eq!(outcome, RecoveryEntryOutcome::Restored);
    assert_eq!(m.step(), OnboardingStep::HandleEntry);
    assert_eq!(
        m.current_handle(),
        "alice@fauna.test",
        "the payload's handle is carried into the field handle_entry asks for next"
    );
    assert_eq!(m.error_message(), None);

    let args = fake.restore_escrowed_seed_args();
    assert_eq!(args.len(), 1);
    let (base_url, secret, actor, handle) = &args[0];
    assert_eq!(
        base_url, "https://fauna.test",
        "the nest is resolved from the handle's domain — the only half that can locate one"
    );
    assert_eq!(secret, &secret_hex());
    assert_eq!(
        actor.as_deref(),
        Some(actor_hex().as_str()),
        "the payload's actor id stays authoritative — the blob is AAD-bound to it"
    );
    assert_eq!(
        handle.as_deref(),
        Some("alice"),
        "the bare local part reaches the wire: the nest stores handles without @domain"
    );
}

/// The onboarding-minted kit carries an actor but no handle (none is chosen yet
/// at that screen's position), so the account field is what locates the nest.
#[tokio::test]
async fn a_handle_less_kit_uses_the_typed_account_to_find_the_nest() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());
    m.set_current_handle("bob@other.test".into());

    let uri = fauna_core::recovery::RecoveryKitQr::to_uri(&secret_hex(), Some(&actor_hex()), None);
    assert_eq!(
        m.submit_recovery_entry(uri).await,
        RecoveryEntryOutcome::Restored
    );

    let (base_url, _, actor, handle) = fake.restore_escrowed_seed_args()[0].clone();
    assert_eq!(base_url, "https://other.test");
    assert_eq!(actor.as_deref(), Some(actor_hex().as_str()));
    assert_eq!(handle.as_deref(), Some("bob"));
}

/// A typed account overrides the payload's handle: a user who names an account
/// is correcting or re-pointing what the phrase said (a handle can change after
/// the kit was written down).
#[tokio::test]
async fn a_typed_account_wins_over_the_payloads_handle() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());
    m.set_current_handle("  alice@moved.test  ".into());

    assert_eq!(
        m.submit_recovery_entry(full_kit_uri()).await,
        RecoveryEntryOutcome::Restored
    );

    let (base_url, ..) = fake.restore_escrowed_seed_args()[0].clone();
    assert_eq!(
        base_url, "https://moved.test",
        "surrounding whitespace must not defeat the override"
    );
    assert_eq!(m.current_handle(), "alice@moved.test");
}

/// A bare 64-hex secret names nothing, so the account field carries the whole
/// burden — and then the nest resolves the actor from the handle.
#[tokio::test]
async fn a_bare_hex_phrase_restores_when_the_account_field_names_the_handle() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());
    m.set_current_handle("alice@fauna.test".into());

    assert_eq!(
        m.submit_recovery_entry(secret_hex()).await,
        RecoveryEntryOutcome::Restored
    );

    let (_, _, actor, handle) = fake.restore_escrowed_seed_args()[0].clone();
    assert_eq!(
        actor, None,
        "nothing named the account, so the nest must resolve it from the handle"
    );
    assert_eq!(handle.as_deref(), Some("alice"));
}

/// The `"nest"` provider override wins over the resolved URL — resolved
/// **machine-side**, exactly as `run_handle_check_phases` does it.
///
/// Not a test-only nicety: `WsNestApi` captures the override map once at
/// construction, while the E2E bridge installs it at runtime, so a caller that
/// left the job to the transport would dial the honest `https://` URL at a
/// harness nest serving plain HTTP. That is precisely how this shipped broken
/// for one run, and the tier_3 journey caught it as a corrupt-message WS
/// handshake.
#[tokio::test]
async fn the_nest_override_wins_over_the_resolved_url() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());
    let mut urls = std::collections::HashMap::new();
    urls.insert("nest".to_string(), "http://127.0.0.1:9123".to_string());
    m.set_provider_base_urls(urls);

    assert_eq!(
        m.submit_recovery_entry(full_kit_uri()).await,
        RecoveryEntryOutcome::Restored
    );

    let (base_url, ..) = fake.restore_escrowed_seed_args()[0].clone();
    assert_eq!(
        base_url, "http://127.0.0.1:9123",
        "the runtime override must reach the ceremony, not just the transport's \
         construction-time copy"
    );
}

// ── Local refusals: nothing is sent ─────────────────────────────────────────

/// The identity-secret confusion is the mistake worth catching locally: the two
/// codecs are deliberate twins on different hosts, and `fauna://identity` must
/// never be taken for a recovery kit.
#[tokio::test]
async fn an_identity_payload_is_refused_before_anything_is_sent() {
    let (m, fake) = machine_with_fake();

    let uri = format!("fauna://identity?secret={}", secret_hex());
    assert_eq!(
        m.submit_recovery_entry(uri).await,
        RecoveryEntryOutcome::InvalidKit
    );
    assert!(
        fake.restore_escrowed_seed_args().is_empty(),
        "a local parse refusal must not open a connection"
    );
    assert_eq!(
        m.step(),
        OnboardingStep::RecoveryEntry,
        "the user stays here to fix it"
    );
    assert_eq!(
        m.error_message(),
        None,
        "the machine holds no string table — the message on `error-message` is the \
         client's localized resolution of the outcome, and a second untranslated \
         owner on that channel is exactly what this pins out"
    );
}

/// A bare hex secret with an empty account field identifies no account, and
/// guessing is not an option — the blob is AAD-bound to the actor.
#[tokio::test]
async fn a_phrase_that_names_no_account_asks_for_one() {
    let (m, fake) = machine_with_fake();

    assert_eq!(
        m.submit_recovery_entry(secret_hex()).await,
        RecoveryEntryOutcome::AccountNeeded
    );
    assert!(fake.restore_escrowed_seed_args().is_empty());
    assert_eq!(m.step(), OnboardingStep::RecoveryEntry);
}

/// A local part alone has no domain, so there is no nest to reach. This is the
/// case `handle_entry` tolerates (a local nest may already be in hand) and the
/// restore cannot: the ceremony is pre-identity, with no session to ask.
#[tokio::test]
async fn an_account_without_a_domain_is_refused_locally() {
    let (m, fake) = machine_with_fake();
    m.set_current_handle("alice".into());

    assert_eq!(
        m.submit_recovery_entry(secret_hex()).await,
        RecoveryEntryOutcome::AccountMalformed
    );
    assert!(fake.restore_escrowed_seed_args().is_empty());
}

// ── The locator of last resort: a direct nest address ───────────────────────
//
// `onboarding.md` § 1 Identity → `recovery_entry` → *The locator of last resort
// is a direct nest address* (ratified 2026-08-11). The account field also takes
// `handle@<direct nest address>` for the one case recovery exists for: the
// handle's domain is dead (lapsed or seized) while the escrow blob, the actor
// and the nest are all intact. The contract splits the field's two jobs — the
// **address part locates** (probed verbatim, exactly as `handle_entry` probes a
// LAN target), and the **handle part names** (actor resolution passes the bare
// handle, never the typed compound). These four pin both jobs across the host
// classes `fauna_provisioning::probe::resolve_handle_domain` recognizes.

/// A LAN address with an explicit port. Nothing about the address may reach the
/// naming half: `fauna.actor.by_handle` matches `users.handle` exactly, so a
/// compound would resolve to no actor at all.
#[tokio::test]
async fn a_direct_lan_address_locates_while_the_bare_handle_names() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());
    m.set_current_handle("alice@192.168.1.5:8443".into());

    let uri = fauna_core::recovery::RecoveryKitQr::to_uri(&secret_hex(), None, None);
    assert_eq!(
        m.submit_recovery_entry(uri).await,
        RecoveryEntryOutcome::Restored
    );

    let (base_url, _, actor, handle) = fake.restore_escrowed_seed_args()[0].clone();
    assert_eq!(
        base_url, "https://192.168.1.5:8443",
        "the address part is probed verbatim — it is the whole point of the fallback"
    );
    assert_eq!(
        actor, None,
        "no actor in the kit, so the nest resolves it from the handle"
    );
    assert_eq!(
        handle.as_deref(),
        Some("alice"),
        "the bare handle names; the typed compound never reaches by_handle"
    );
}

/// An **unbracketed** IPv6 literal — what a user reads off a router — must come
/// back bracketed. This is the case that proves the address goes through the
/// shared probe classification rather than a bare `https://{typed}`: without the
/// ≥2-colon rule and the re-bracket, `https://2001:db8::5` is a URL whose
/// authority a parser reads as host `2001` port `db8`, so the ceremony would
/// dial somewhere else entirely (`security.md` § Transport trust).
#[tokio::test]
async fn a_direct_ipv6_address_locates() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());
    m.set_current_handle("alice@2001:db8::5".into());

    assert_eq!(
        m.submit_recovery_entry(secret_hex()).await,
        RecoveryEntryOutcome::Restored
    );

    let (base_url, _, _, handle) = fake.restore_escrowed_seed_args()[0].clone();
    assert_eq!(base_url, "https://[2001:db8::5]");
    assert_eq!(handle.as_deref(), Some("alice"));
}

/// An mDNS name — the "my box is the one on this LAN" address a user can say
/// out loud. Classified non-public, so no unicast-DNS probe is attempted.
#[tokio::test]
async fn a_direct_mdns_address_locates() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());
    m.set_current_handle("alice@pi.local".into());

    assert_eq!(
        m.submit_recovery_entry(secret_hex()).await,
        RecoveryEntryOutcome::Restored
    );

    let (base_url, _, _, handle) = fake.restore_escrowed_seed_args()[0].clone();
    assert_eq!(base_url, "https://pi.local");
    assert_eq!(handle.as_deref(), Some("alice"));
}

/// Loopback with an explicit port — the same-box case, and the shape the tier_3
/// journey dials. `https` like every other origin (the scheme does not depend on
/// host class).
#[tokio::test]
async fn a_direct_loopback_address_locates() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());
    m.set_current_handle("alice@127.0.0.1:9443".into());

    assert_eq!(
        m.submit_recovery_entry(secret_hex()).await,
        RecoveryEntryOutcome::Restored
    );

    let (base_url, _, _, handle) = fake.restore_escrowed_seed_args()[0].clone();
    assert_eq!(base_url, "https://127.0.0.1:9443");
    assert_eq!(handle.as_deref(), Some("alice"));
}

/// A loopback address that names no port takes the injected local-nest port —
/// the same-box seam `set_local_nest_port` owns, and what a tier_3 nest on a
/// random free port needs. The second discriminator that a naive
/// `https://{typed}` cannot produce.
#[tokio::test]
async fn a_portless_loopback_address_takes_the_injected_local_port() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());
    m.set_local_nest_port(9443);
    m.set_current_handle("alice@localhost".into());

    assert_eq!(
        m.submit_recovery_entry(secret_hex()).await,
        RecoveryEntryOutcome::Restored
    );

    let (base_url, _, _, handle) = fake.restore_escrowed_seed_args()[0].clone();
    assert_eq!(base_url, "https://localhost:9443");
    assert_eq!(handle.as_deref(), Some("alice"));
}

// ── The nest's answers ──────────────────────────────────────────────────────

/// `fauna.recovery.no_escrow` is the ratified honest signal, not a fault: the
/// user stays on the screen and is told to re-create the kit from a signed-in
/// device. It must never be collapsed into a generic failure.
#[tokio::test]
async fn no_escrow_leaves_the_user_here_with_the_honest_answer() {
    let (m, _fake) = machine_with_fake();
    // The fake's benign default IS this refusal — a fresh nest holds no blob.

    assert_eq!(
        m.submit_recovery_entry(full_kit_uri()).await,
        RecoveryEntryOutcome::NoEscrow
    );
    assert_eq!(m.step(), OnboardingStep::RecoveryEntry);
    assert_eq!(
        m.current_handle(),
        "",
        "a refused restore commits nothing — not even the account it tried"
    );
}

/// `fauna.auth.superseded` routes to the identity import, and the machine
/// deliberately does **not** carry the successor: the refusal's claim is
/// unverified, and this client does not repeat an unproven claim
/// (`identity-succession.md` § Propagation). The routing itself is the client's,
/// uniform with the launch flow's refusal — so the machine leaves the step alone.
#[tokio::test]
async fn a_superseded_identity_is_handed_to_the_client_without_naming_the_successor() {
    let (m, fake) = machine_with_fake();
    fake.set_restore_escrowed_seed_response(Err(RestoreSeedError::Superseded {
        successor: actor_hex(),
    }));

    let outcome = m.submit_recovery_entry(full_kit_uri()).await;

    assert_eq!(outcome, RecoveryEntryOutcome::Superseded);
    assert!(
        !format!("{outcome:?}").contains(&actor_hex()),
        "the unverified successor must not ride out of the machine"
    );
    assert_eq!(
        m.step(),
        OnboardingStep::RecoveryEntry,
        "the client owns the routing, exactly as it does for the launch refusal"
    );
}

/// A handle that resolves to a nest which knows no such account is the account
/// field being wrong — distinct from "no blob rests", because the remedy differs.
#[tokio::test]
async fn an_unknown_account_names_what_was_wrong() {
    let (m, fake) = machine_with_fake();
    fake.set_restore_escrowed_seed_response(Err(RestoreSeedError::AccountUnknown {
        handle: "alice".into(),
    }));

    assert_eq!(
        m.submit_recovery_entry(full_kit_uri()).await,
        RecoveryEntryOutcome::AccountUnknown {
            account: "alice".into()
        }
    );
}

/// A retired kit still signs structurally valid bytes; they simply authorize
/// nothing now. The screen says so rather than offering a pointless retry.
#[tokio::test]
async fn a_retired_kit_is_refused_not_retried() {
    let (m, fake) = machine_with_fake();
    fake.set_restore_escrowed_seed_response(Err(RestoreSeedError::Refused {
        reason: "the recovery signature was refused".into(),
    }));

    assert!(matches!(
        m.submit_recovery_entry(full_kit_uri()).await,
        RecoveryEntryOutcome::Refused { .. }
    ));
}

/// Reachability is the one bucket a retry helps, so it stays distinguishable
/// from every verdict about the kit.
#[tokio::test]
async fn an_unreachable_nest_stays_distinguishable_from_a_refusal() {
    let (m, fake) = machine_with_fake();
    fake.set_restore_escrowed_seed_response(Err(RestoreSeedError::Transient {
        reason: "connection refused".into(),
    }));

    assert!(matches!(
        m.submit_recovery_entry(full_kit_uri()).await,
        RecoveryEntryOutcome::Unreachable { .. }
    ));
}

// ── The predecessor section (`identity-succession.md` § Seed escrow) ─────────

/// **A restore inside a succession's re-seal window carries the predecessor
/// seeds up to the client, which is the only party that can persist them.**
///
/// This machine holds no store, exactly as it holds none for `imported_secret`
/// — so an unread value is simply lost when the wizard ends, and the getter is
/// the whole handoff. Without it the seeds are unsealed and then dropped, and
/// the corpus they exist to open stays unopenable.
#[tokio::test]
async fn a_restore_hands_recovered_predecessor_seeds_to_the_client() {
    let (m, fake) = machine_with_fake();
    fake.set_restore_escrowed_seed_response(Ok(RestoredIdentity {
        seed_hex: restored_seed_hex(),
        predecessors: vec![RestoredPredecessorSeed {
            actor_id_hex: "a1".repeat(32),
            seed_hex: "cd".repeat(32).into(),
        }],
        predecessors_unreadable: None,
    }));

    let outcome = m.submit_recovery_entry(full_kit_uri()).await;

    assert_eq!(
        outcome,
        RecoveryEntryOutcome::Restored,
        "a healthy predecessor section changes nothing about the outcome"
    );
    let carried = m.restored_predecessors();
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].actor_id_hex, "a1".repeat(32));
    assert_eq!(carried[0].seed_hex.as_str(), "cd".repeat(32));
    assert_eq!(m.restored_predecessors_unreadable(), None);
}

/// **An ordinary restore carries none — and that is not the same state as a
/// section that failed to open.**
#[tokio::test]
async fn an_ordinary_restore_carries_no_predecessors_and_reports_no_loss() {
    let (m, fake) = machine_with_fake();
    fake.set_restored_seed(&restored_seed_hex());

    assert_eq!(
        m.submit_recovery_entry(full_kit_uri()).await,
        RecoveryEntryOutcome::Restored
    );
    assert!(m.restored_predecessors().is_empty());
    assert_eq!(
        m.restored_predecessors_unreadable(),
        None,
        "no section is not a lost section — a screen branches on the difference"
    );
}

/// **A broken predecessor section does NOT fail the restore, and does NOT pass
/// silently.**
///
/// Both halves are load-bearing and pull opposite ways. The account must come
/// back — the auxiliary never costs the primary — but this is the one path
/// where a *successful* recovery still lost user-irrecoverable data, so the
/// outcome is a distinct variant a client cannot render without deciding what
/// to say. Flattening it into `Restored` would be the silence that matters.
#[tokio::test]
async fn a_lost_predecessor_section_still_restores_but_says_so() {
    let (m, fake) = machine_with_fake();
    fake.set_restore_escrowed_seed_response(Ok(RestoredIdentity {
        seed_hex: restored_seed_hex(),
        predecessors: Vec::new(),
        predecessors_unreadable: Some("hpke open failed".into()),
    }));

    let outcome = m.submit_recovery_entry(full_kit_uri()).await;

    assert!(
        matches!(
            outcome,
            RecoveryEntryOutcome::RestoredPredecessorsLost { .. }
        ),
        "the loss is reported, not flattened into Restored: {outcome:?}"
    );
    assert_eq!(
        m.step(),
        OnboardingStep::HandleEntry,
        "the account IS back — the wizard advances exactly as a clean restore does"
    );
    assert_eq!(
        m.effective_secret().as_deref(),
        Some(restored_seed_hex().as_str()),
        "and the recovered seed is held, so this can never read as a failed recovery"
    );
    assert_eq!(
        m.restored_predecessors_unreadable().as_deref(),
        Some("hpke open failed")
    );
}

/// Every outcome that stays on `recovery_entry` (or lands on `handle_entry`
/// with something the user must hear) carries the ONE message all seven apps
/// render — the `onboarding.recovery_entry.*` key the variant's own doc names,
/// with its argument — so no app re-derives the table. The two that route
/// instead of speaking carry none: `Restored` is silent success, and
/// `Superseded` is the client's routing to the import screen.
#[test]
fn each_outcome_names_the_one_message_every_app_renders() {
    use RecoveryEntryOutcome as O;
    use fauna_core::localized::LocalizedText as L;
    let r = || "why".to_string();
    let cases = [
        (O::Restored, None),
        (O::Superseded, None),
        (
            O::RestoredPredecessorsLost { reason: r() },
            Some(L::key_arg(
                "onboarding.recovery_entry.restored_predecessors_lost",
                "reason",
                "why",
            )),
        ),
        (
            O::InvalidKit,
            Some(L::key("onboarding.recovery_entry.invalid_kit")),
        ),
        (
            O::AccountNeeded,
            Some(L::key("onboarding.recovery_entry.account_needed")),
        ),
        (
            O::AccountMalformed,
            Some(L::key("onboarding.recovery_entry.account_malformed")),
        ),
        (
            O::AccountUnknown {
                account: "ada@nest.example".into(),
            },
            Some(L::key_arg(
                "onboarding.recovery_entry.account_unknown",
                "account",
                "ada@nest.example",
            )),
        ),
        (
            O::NoEscrow,
            Some(L::key("onboarding.recovery_entry.no_escrow")),
        ),
        (
            O::Refused { reason: r() },
            Some(L::key_arg(
                "onboarding.recovery_entry.refused",
                "reason",
                "why",
            )),
        ),
        (
            O::Unreachable { reason: r() },
            Some(L::key_arg(
                "onboarding.recovery_entry.unreachable",
                "reason",
                "why",
            )),
        ),
    ];
    for (outcome, expected) in cases {
        assert_eq!(outcome.message(), expected, "{outcome:?}");
    }
}
