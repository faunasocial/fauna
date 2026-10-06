//! TEST-ONLY helper: the client-side recovery crypto an e2e fixture needs.
//!
//! The e2e harness has to arrange two **preconditions** before it can test what
//! it actually cares about — that a succeeded device is refused on its next
//! connect and lands on the import flow (`docs/goal/behavior/identity-
//! succession.md` § Propagation → *Own device fleet*): the identity must have a
//! registered RecoveryKey, and it must then actually be succeeded. Neither is
//! the behavior under test, which is why arranging them outside the app UI is
//! within convention 8's fixture-setup carve-out.
//!
//! Doing both here rather than through the app is also what keeps the journey
//! **app-agnostic**. The Settings → Recovery kit ceremony has landed on tui
//! only; driving kit creation through it would gate a journey about the *old
//! device's refusal* on an unrelated per-app UI leg, so the test would skip on
//! six apps for a reason that has nothing to do with what it asserts.
//!
//! ## Why Rust rather than Python
//!
//! Both records are canonical DAG-CBOR signed under domain-separation tags, and
//! the nest verifies them for real — a fabricated succession row would not
//! verify, so the client could never name the successor and the journey's
//! load-bearing positive assertion could not exist. Reimplementing that in
//! Python would duplicate shared-Rust crypto (priority #2) and rot silently the
//! moment a record's shape changed. So the split is: **this helper owns every
//! wire shape, Python owns only transport.** Python shuttles opaque hex and
//! knows nothing about the contents.
//!
//! Each subcommand mirrors the construction of the ceremony it stands in for —
//! `create_kit_with_root` and `succeed_identity` respectively — minus the
//! transport, which is precisely the part Python is doing. Kept in step with
//! them: if a record's shape changes, this changes with it.
//!
//! An `examples/` binary on purpose — it adds no workspace member (so no Docker
//! crate-list drift) and is never built into a shipped artifact.
//!
//! ## Contract
//!
//! Everything is hex, in and out, so no encoding dependency is needed. Output is
//! `key=value` lines on stdout.
//!
//! ```text
//! recovery_fixture register-kit \
//!   --identity-seed <64 hex> \
//!   [--recovery-secret <64 hex>]      # minted when omitted
//! → recovery_secret=<64 hex>
//!   registration=<hex>                # submit via fauna.recovery.registration.submit
//!
//! recovery_fixture mint-succession \
//!   --old-actor-id <64 hex> \
//!   --recovery-secret <64 hex> \
//!   --successor-seed <64 hex> \
//!   [--old-seed <64 hex>] \
//!   --registration <hex> [--registration <hex> ...]
//! → new_actor_id=<64 hex>
//!   statement=<hex>                   # submit via fauna.recovery.succession.submit
//!
//! recovery_fixture mint-seed-alone-replacement \
//!   --identity-seed <64 hex> \
//!   --registration <hex> [--registration <hex> ...]
//! → recovery_secret=<64 hex>          # the key that WOULD be registered
//!   registration=<hex>                # submit via fauna.recovery.replacement.request
//!
//! recovery_fixture sign-veto \
//!   --old-actor-id <64 hex> \
//!   --recovery-secret <64 hex> \
//!   --nonce <64 hex>                  # from fauna.recovery.replacement.challenge
//! → signature=<hex>                   # submit via fauna.recovery.replacement.veto
//!
//! recovery_fixture sign-profile \
//!   --identity-seed <64 hex> \
//!   --display-name <hex of UTF-8> \
//!   --bio <hex of UTF-8>
//! → body=<hex>                        # submit via fauna.profile.set
//!
//! recovery_fixture decode-profile \
//!   --body <hex>                      # fauna.profile.get's reply body
//! → actor_id=<64 hex>                 # the identity the stored bytes name
//!   origin=direct|delegated|unsigned  # who signed them (the envelope's verdict)
//!   display_name=<hex of UTF-8>       # empty when absent
//!   bio=<hex of UTF-8>                # empty when absent
//! ```
//!
//! The two profile subcommands exist for the journeys about a successor's
//! *inherited* profile row (`profile.md` § After an identity succession). A
//! succession moves the row's ownership but not its signed bytes, so the
//! precondition is a profile the **predecessor** signed, and the outcome is
//! read off the stored bytes' signer. `sign-profile` mirrors the edit form's
//! first publish (`build_edited_profile` over no base). `decode-profile` is
//! `decode_profile`, the one signed-only verify read every app runs, so Python
//! never learns the envelope's shape.
//!
//! `--registration` is repeated once per entry of `fauna.recovery.registration
//! .chain`'s reply, in the order the nest served them. The head's `seq` and
//! `recovery_pubkey` are read from the last one — the same `chain_head`
//! derivation `RecoveryClient` performs, done here so Python never decodes a
//! registration record.
//!
//! `register-kit` deliberately supports only a **first** registration (empty
//! chain, `seq` 1). Re-keying an existing chain needs the prior kit and has
//! real arm-selection rules (`kit.rs::next_seq`); a fixture that guessed at
//! them would be a second, wrong implementation of a ceremony that already
//! exists.

use fauna_core::data::Timestamp;
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::recovery::{
    IdentitySuccession, RecoveryKey, RecoveryKeyRegistration, ReplacementVeto,
    SignedRecoveryKeyRegistration,
};

fn main() {
    if let Err(e) = run() {
        eprintln!("recovery_fixture: {e}");
        std::process::exit(1);
    }
}

#[derive(Default)]
struct Args {
    identity_seed: Option<String>,
    old_actor_id: Option<String>,
    recovery_secret: Option<String>,
    successor_seed: Option<String>,
    old_seed: Option<String>,
    nonce: Option<String>,
    registrations: Vec<String>,
    display_name: Option<String>,
    bio: Option<String>,
    body: Option<String>,
}

fn run() -> Result<(), String> {
    let mut argv = std::env::args().skip(1);
    let subcommand = argv.next().ok_or(
        "expected a subcommand: register-kit | mint-succession | \
         mint-seed-alone-replacement | sign-veto | sign-profile | decode-profile",
    )?;

    let mut args = Args::default();
    while let Some(flag) = argv.next() {
        let value = argv.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--identity-seed" => args.identity_seed = Some(value),
            "--old-actor-id" => args.old_actor_id = Some(value),
            "--recovery-secret" => args.recovery_secret = Some(value),
            "--successor-seed" => args.successor_seed = Some(value),
            "--old-seed" => args.old_seed = Some(value),
            "--nonce" => args.nonce = Some(value),
            "--registration" => args.registrations.push(value),
            "--display-name" => args.display_name = Some(value),
            "--bio" => args.bio = Some(value),
            "--body" => args.body = Some(value),
            other => return Err(format!("unknown argument {other}")),
        }
    }

    match subcommand.as_str() {
        "register-kit" => register_kit(args),
        "mint-succession" => mint_succession(args),
        "mint-seed-alone-replacement" => mint_seed_alone_replacement(args),
        "sign-veto" => sign_veto(args),
        "sign-profile" => sign_profile(args),
        "decode-profile" => decode_profile(args),
        other => Err(format!(
            "unknown subcommand {other}: expected register-kit | mint-succession | \
             mint-seed-alone-replacement | sign-veto | sign-profile | decode-profile"
        )),
    }
}

/// Mirrors `create_kit_with_root`'s record construction, first-registration arm.
fn register_kit(args: Args) -> Result<(), String> {
    let identity_seed = args.identity_seed.ok_or("--identity-seed is required")?;
    let identity = ActorKeypair::from_secret_hex(identity_seed.trim())
        .map_err(|e| format!("--identity-seed: {e}"))?;

    if !args.registrations.is_empty() {
        return Err(
            "this identity already has a registration chain; re-keying needs the \
             prior kit and belongs to the real ceremony, not a fixture"
                .to_string(),
        );
    }

    let recovery = match args.recovery_secret {
        Some(hex) => {
            RecoveryKey::from_hex(hex.trim()).map_err(|e| format!("--recovery-secret: {e}"))?
        }
        None => RecoveryKey::generate(),
    };

    let registration = RecoveryKeyRegistration {
        actor_id: identity.actor_id(),
        recovery_pubkey: recovery.public(),
        // `next_seq(None, None)` — the first kit on an empty chain.
        seq: 1,
        created_at: Timestamp::now(),
    };
    let signed = registration
        .sign(identity.signing_key(), &recovery, None)
        .map_err(|e| format!("signing the registration: {e}"))?;
    let bytes = canonical_encode(&signed).map_err(|e| format!("encoding the registration: {e}"))?;

    println!("recovery_secret={}", recovery.to_hex());
    println!("registration={}", encode_hex(&bytes));
    Ok(())
}

/// Mirrors `succeed_identity`'s statement construction.
fn mint_succession(args: Args) -> Result<(), String> {
    let old_actor_id = args.old_actor_id.ok_or("--old-actor-id is required")?;
    let recovery_secret = args
        .recovery_secret
        .ok_or("--recovery-secret is required")?;
    let successor_seed = args.successor_seed.ok_or("--successor-seed is required")?;

    let old_actor_id =
        ActorId(decode_32(&old_actor_id).map_err(|e| format!("--old-actor-id: {e}"))?);
    let recovery = RecoveryKey::from_hex(recovery_secret.trim())
        .map_err(|e| format!("--recovery-secret: {e}"))?;
    let successor = ActorKeypair::from_secret_hex(successor_seed.trim())
        .map_err(|e| format!("--successor-seed: {e}"))?;
    let old_identity = match args.old_seed {
        Some(hex) => Some(
            ActorKeypair::from_secret_hex(hex.trim()).map_err(|e| format!("--old-seed: {e}"))?,
        ),
        None => None,
    };

    // The chain head, derived exactly as `RecoveryClient::chain_head` does: the
    // LAST registration the nest served, decoded here so the harness never has
    // to know what a registration record looks like.
    let head = args
        .registrations
        .last()
        .ok_or("at least one --registration is required (the identity registered no kit?)")?;
    let head_bytes = decode_hex(head).map_err(|e| format!("--registration: {e}"))?;
    let head: SignedRecoveryKeyRegistration = canonical_decode(&head_bytes)
        .map_err(|e| format!("decoding the head registration record: {e}"))?;

    // The same local pre-check the ceremony makes: a statement signed under a
    // kit the chain does not name is one the nest must refuse, and a fixture
    // that submitted it would fail far from its cause.
    if recovery.public() != head.registration.recovery_pubkey {
        return Err(
            "the supplied kit is not the one this identity's chain names \
             (recovery pubkey mismatch)"
                .to_string(),
        );
    }

    let statement = IdentitySuccession {
        old_actor_id,
        new_actor_id: successor.actor_id(),
        recovery_pubkey: head.registration.recovery_pubkey,
        seq: head.registration.seq + 1,
        created_at: Timestamp::now(),
    };
    let signed = statement
        .sign(
            &recovery,
            successor.signing_key(),
            old_identity.as_ref().map(|k| k.signing_key()),
        )
        .map_err(|e| format!("signing the succession statement: {e}"))?;
    let bytes =
        canonical_encode(&signed).map_err(|e| format!("encoding the succession statement: {e}"))?;

    println!("new_actor_id={}", encode_hex(&successor.actor_id().0));
    println!("statement={}", encode_hex(&bytes));
    Ok(())
}

/// Mirrors `replacement::request_seed_alone_replacement`'s record construction.
///
/// The seed-alone arm: `prior = None`, so the record carries no co-signature by
/// the key being replaced — which is the whole point (that key is the one that
/// was lost). The strict `verify` deliberately still refuses it; only
/// `verify_seed_alone` accepts, and only into the pending store.
fn mint_seed_alone_replacement(args: Args) -> Result<(), String> {
    let identity_seed = args.identity_seed.ok_or("--identity-seed is required")?;
    let identity = ActorKeypair::from_secret_hex(identity_seed.trim())
        .map_err(|e| format!("--identity-seed: {e}"))?;

    // Same `chain_head` derivation as `mint-succession`: the last record the
    // nest served. A seed-alone request must ADVANCE that head, so the seq is
    // read from the chain rather than guessed.
    let head = args.registrations.last().ok_or(
        "at least one --registration is required — a seed-alone replacement \
         replaces a REGISTERED key, and an identity with an empty chain has \
         nothing to replace (the nest refuses it with `not_registered`)",
    )?;
    let head_bytes = decode_hex(head).map_err(|e| format!("--registration: {e}"))?;
    let head: SignedRecoveryKeyRegistration = canonical_decode(&head_bytes)
        .map_err(|e| format!("decoding the head registration record: {e}"))?;

    let recovery = match args.recovery_secret {
        Some(hex) => {
            RecoveryKey::from_hex(hex.trim()).map_err(|e| format!("--recovery-secret: {e}"))?
        }
        None => RecoveryKey::generate(),
    };
    let registration = RecoveryKeyRegistration {
        actor_id: identity.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: head.registration.seq + 1,
        created_at: Timestamp::now(),
    };
    let signed = registration
        .sign(identity.signing_key(), &recovery, None)
        .map_err(|e| format!("signing the seed-alone registration: {e}"))?;
    let bytes = canonical_encode(&signed)
        .map_err(|e| format!("encoding the seed-alone registration: {e}"))?;

    println!("recovery_secret={}", recovery.to_hex());
    println!("registration={}", encode_hex(&bytes));
    Ok(())
}

/// Mirrors `replacement::veto_pending_replacement`'s signature construction.
///
/// The nonce comes from `fauna.recovery.replacement.challenge` and is
/// single-use, so this signature authorizes exactly one veto — which is what
/// stops a captured veto cancelling a future honest replacement forever.
fn sign_veto(args: Args) -> Result<(), String> {
    let actor_id = args
        .old_actor_id
        .ok_or("--old-actor-id is required (the account whose pending is contested)")?;
    let recovery_secret = args
        .recovery_secret
        .ok_or("--recovery-secret is required")?;
    let nonce = args.nonce.ok_or("--nonce is required")?;

    let actor_id = ActorId(decode_32(&actor_id).map_err(|e| format!("--old-actor-id: {e}"))?);
    let recovery = RecoveryKey::from_hex(recovery_secret.trim())
        .map_err(|e| format!("--recovery-secret: {e}"))?;
    let nonce = decode_32(&nonce).map_err(|e| format!("--nonce: {e}"))?;

    let signature = ReplacementVeto::new(actor_id, nonce)
        .sign(&recovery)
        .map_err(|e| format!("signing the veto: {e}"))?;

    println!("signature={}", encode_hex(&signature));
    Ok(())
}

/// Mirrors the profile edit form's first publish: `build_edited_profile` over no
/// base, so the record carries the same minimal defaults a real first save does.
fn sign_profile(args: Args) -> Result<(), String> {
    let identity_seed = args.identity_seed.ok_or("--identity-seed is required")?;
    let identity = ActorKeypair::from_secret_hex(identity_seed.trim())
        .map_err(|e| format!("--identity-seed: {e}"))?;
    let display_name = decode_text(args.display_name.as_deref(), "--display-name")?;
    let bio = decode_text(args.bio.as_deref(), "--bio")?;

    let body =
        fauna_client_profile::build_edited_profile(&identity, None, &[], display_name, bio, vec![])
            .map_err(|e| format!("signing the profile: {e}"))?;

    println!("body={}", encode_hex(&body));
    Ok(())
}

/// `decode_profile` — the signed-only verify-then-decode every writer's base goes through.
fn decode_profile(args: Args) -> Result<(), String> {
    let body = args.body.ok_or("--body is required")?;
    let body = decode_hex(&body).map_err(|e| format!("--body: {e}"))?;
    let (profile, origin) = fauna_client_profile::decode_profile(&body)
        .map_err(|e| format!("decoding the stored profile: {e}"))?;
    let origin = match origin {
        fauna_core::encoding::AuthoringOrigin::Direct => "direct",
        fauna_core::encoding::AuthoringOrigin::Delegated { .. } => "delegated",
    };

    println!("actor_id={}", encode_hex(&profile.actor_id.0));
    println!("origin={origin}");
    println!(
        "display_name={}",
        encode_hex(profile.display_name.unwrap_or_default().as_bytes())
    );
    println!(
        "bio={}",
        encode_hex(profile.bio.unwrap_or_default().as_bytes())
    );
    Ok(())
}

/// A text argument, carried as the hex of its UTF-8 so every value on the
/// command line stays hex. Absent means the field is left unset.
fn decode_text(arg: Option<&str>, flag: &str) -> Result<Option<String>, String> {
    let Some(hex) = arg else {
        return Ok(None);
    };
    let bytes = decode_hex(hex).map_err(|e| format!("{flag}: {e}"))?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| format!("{flag}: not UTF-8: {e}"))
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    fauna_core::format::hex_decode(s.trim()).ok_or_else(|| "invalid hex".to_string())
}

fn decode_32(s: &str) -> Result<[u8; 32], String> {
    let bytes = decode_hex(s)?;
    bytes
        .try_into()
        .map_err(|_| "expected 32 bytes (64 hex chars)".to_string())
}

fn encode_hex(bytes: &[u8]) -> String {
    fauna_core::format::hex_full(bytes)
}
