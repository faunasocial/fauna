//! E2e test helper: mint a real MLS group + Welcome for a recipient's key package.
//!
//! Used by `tests/e2e-unified/tests/test_fauna_mls_web_receive.py` to prove the
//! **web FaunaMls receive loop** (durable inbox-apply, layer 4). That proof needs
//! a Welcome the web app can actually *join* — i.e. a real MLS group created by
//! a second engine against the web user's published key package. The web user
//! (bob) is the only real GUI engine in the test; the *sender* (alice) has no GUI
//! app, so — exactly like the sibling `mls-keypackage-gen` helper does for
//! API-tier peers — this helper spins a throwaway in-memory engine bound to
//! alice's identity, parses bob's TLS-serialized key package, creates a 1:1 group,
//! and emits the resulting `(channel_id, Welcome)`. Alice's group state lives only
//! in the throwaway engine and is discarded on exit (the test never drives alice
//! again — it only observes that bob's web GUI joins). Two-real-engine decrypt is
//! proven separately in `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`.
//!
//! Usage: `mls-group-gen <alice_secret_hex> <bob_keypackage_hex> [message_body...]`
//! Output (stdout, three lines): `<channel_id_hex>`, `<welcome_hex>`, then
//! `<raw_group_id_hex>` — the raw (variable-length) MLS group id the recipient's
//! nest needs to bind a shared folder (`fauna.folders.share` re-derives the
//! same `ChannelId` from it). Older two-line consumers still read lines 1–2.
//!
//! With the optional `[message_body]`, a **fourth** line is emitted: an
//! `Application` [`ChannelEnvelope`] (hex) sealing that body to the group, ready to
//! `fauna.conversations.channel.send`. The recipient is a member from group
//! creation, so it decrypts after they join the Welcome — which is what lets a
//! GUI-receive proof drive a real *decrypt*, not just a join. Used by
//! `test_moderation_local_detection.py`'s web leg: only a decrypted body is
//! classified, so the queue's post-decrypt local half cannot be proven without one.
//! Each further body adds one more line — the next message of the same sender's
//! stream (`sequence` 2, 3, …), so a proof can post them at different moments
//! (the read-state relaunch witness posts the second while the recipient's app
//! is closed).
//!
//! Scheduling mode: `mls-group-gen --scheduling <secret_hex> <keypackage_hex>
//! <imip_hex>` seals the raw RFC 5322 iMIP `imip_hex` as a one-off *scheduling*
//! delivery through [`MlsEngine::build_scheduling_delivery`] — the one builder
//! the Fauna app rail and the MDA gateway both seal with — and prints three
//! lines: `<channel_id_hex>`, `<welcome_hex>`, `<app_envelope_hex>`, ready for
//! `welcome.deliver` (kind `scheduling`) + `channel.send`. Used by the tier_3
//! inbound-scheduling authorization journeys (`test_caldav_autoschedule_mailbox_less.py`)
//! to deliver, as a second actor, a message byte-identical to the organizer's.

use fauna_core::data::Timestamp;
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;
use fauna_mls::types::{ChannelMessage, ChannelMessageBody};

/// `--scheduling <secret_hex> <keypackage_hex> <imip_hex>` — see the module docs.
fn scheduling(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    const USAGE: &str =
        "usage: mls-group-gen --scheduling <secret_hex> <keypackage_hex> <imip_hex>";
    let mut next = || args.next().ok_or_else(|| anyhow::anyhow!(USAGE));
    let secret: [u8; 32] = hex::decode(next()?.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("secret must be 32 bytes"))?;
    let keypackage_bytes = hex::decode(next()?.trim())?;
    let imip = hex::decode(next()?.trim())?;

    let keypair = ActorKeypair::from_secret(secret);
    let sender = keypair.actor_id();
    let engine =
        MlsEngine::new_in_memory(keypair).map_err(|e| anyhow::anyhow!("engine init: {e:?}"))?;
    let delivery = engine
        .build_scheduling_delivery(&keypackage_bytes, sender, imip)
        .map_err(|e| anyhow::anyhow!("build scheduling delivery: {e:?}"))?;
    println!("{}", delivery.channel_id);
    println!("{}", hex::encode(delivery.welcome_bytes));
    println!("{}", hex::encode(delivery.app_envelope));
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("--scheduling") {
        args.next();
        return scheduling(args);
    }
    let secret_hex = args.next().ok_or_else(|| {
        anyhow::anyhow!("usage: mls-group-gen <secret_hex> <keypackage_hex> [message_body...]")
    })?;
    let keypackage_hex = args.next().ok_or_else(|| {
        anyhow::anyhow!("usage: mls-group-gen <secret_hex> <keypackage_hex> [message_body...]")
    })?;
    let message_bodies: Vec<String> = args.collect();

    let secret_bytes = hex::decode(secret_hex.trim())?;
    let secret: [u8; 32] = secret_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("secret must be 32 bytes"))?;
    let keypackage_bytes = hex::decode(keypackage_hex.trim())?;

    let keypair = ActorKeypair::from_secret(secret);
    let self_actor = keypair.actor_id();
    let engine =
        MlsEngine::new_in_memory(keypair).map_err(|e| anyhow::anyhow!("engine init: {e:?}"))?;
    let key_package = engine
        .key_package_from_bytes(&keypackage_bytes)
        .map_err(|e| anyhow::anyhow!("parse key package: {e:?}"))?;
    let (channel_id, welcome) = engine
        .create_group(&[key_package])
        .map_err(|e| anyhow::anyhow!("create group: {e:?}"))?;
    let welcome_bytes = welcome
        .to_bytes()
        .map_err(|e| anyhow::anyhow!("serialize welcome: {e:?}"))?;
    // The raw MLS group id backing the just-created group. `ChannelId` is a
    // one-way BLAKE3 hash of it, so the group id can't be recovered from line 1 —
    // a recipient's nest needs it verbatim to bind a shared folder
    // (`fauna.folders.share` re-derives the identical `ChannelId`).
    let raw_group_id = engine
        .group_id_bytes(&channel_id)
        .ok_or_else(|| anyhow::anyhow!("group id missing for freshly-created group"))?;

    // Line 1: channel id (hex, the form `fauna.conversations.welcome.deliver` and
    // `ChannelId::Display` both use). Line 2: the TLS-serialized Welcome (hex).
    // Line 3: the raw MLS group id (hex).
    println!("{channel_id}");
    println!("{}", hex::encode(welcome_bytes));
    println!("{}", hex::encode(raw_group_id));

    // Lines 4… (optional): one `Application` envelope per body, sealed to the
    // group in order — the same shape `FaunaMlsBackend::post_app_message` posts
    // (`sequence` numbers this throwaway sender's stream from 1; `channel_epoch`
    // is app-level metadata only — the MLS framing carries the crypto epoch).
    for (sequence, body) in (1u64..).zip(message_bodies) {
        let message = ChannelMessage {
            sender: self_actor,
            sequence,
            channel_epoch: 0,
            body: ChannelMessageBody::Text(body),
            timestamp: Timestamp::now(),
        };
        let envelope = engine
            .encrypt_to_envelope(&channel_id, &message)
            .map_err(|e| anyhow::anyhow!("encrypt application message: {e:?}"))?;
        println!("{}", hex::encode(envelope));
    }
    Ok(())
}
