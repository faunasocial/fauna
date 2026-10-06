//! Tests for the WASM mint binding's logic (`fauna_wasm::mint_key_blob_inner`,
//! the testable core of the `#[wasm_bindgen]` `mint_key_blob`). Same
//! author-side path the web subscriptions session calls; asserts the
//! returned `(envelope, bytes)` pair satisfies the nest consumer —
//! `into_signed` → `decode_signed_bytes::<KeyBlob>` → `verify_key_blob_signature`
//! (mirroring `bins/fauna-nest/src/subscription_handlers.rs::verify_encrypted_upload`)
//! — and that each subscriber can unwrap their entry.
//!
//! `fauna_wasm::mint_key_blob_inner` lives behind this crate's
//! `#[cfg(target_arch = "wasm32")]` gate, so these run as `#[wasm_bindgen_test]`
//! under `wasm-pack test`, not as plain `#[test]` under a native `cargo test`.
//! `run_in_browser`, not the wasm-bindgen-test default of Node — this
//! project's toolchain has no Node.js (the web SPA is built with Deno).

use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{EmbedAsBytes, decode_signed_bytes, sign_envelope};
use fauna_core::identity::ActorKeypair;
use fauna_core::subscription::crypto::{
    decrypt_key_blob_entry, decrypt_key_blob_entry_for, subscriber_mlkem_encaps_key,
    verify_key_blob_signature,
};
use fauna_core::subscription::types::{KemSuiteId, KeyBlob};
use fauna_wasm::mint_key_blob_inner;
use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

/// Build a signed `DeviceAuthorization` and return its embed-as-bytes pair
/// `(envelope, bytes)` — the two `signer_auth_*` args the web app passes.
fn signed_auth(
    author: &ActorKeypair,
    device: &ActorKeypair,
    caps: Vec<Capability>,
) -> (Vec<u8>, Vec<u8>) {
    let auth = DeviceAuthorization {
        actor_id: author.actor_id(),
        device_key: device.actor_id().0,
        capabilities: caps,
        created_at: Timestamp(1_700_000_000_000_000),
        expires_at: None,
    };
    let (bytes, env) = sign_envelope(author, &auth).expect("sign auth");
    let wire = EmbedAsBytes::from_signed(bytes, env);
    (wire.envelope, wire.bytes)
}

/// Concatenate subscriber `ActorId`s into the `32·N` flat-bytes roster arg.
fn roster<'a>(subs: impl IntoIterator<Item = &'a ActorKeypair>) -> Vec<u8> {
    let mut out = Vec::new();
    for s in subs {
        out.extend_from_slice(&s.actor_id().0);
    }
    out
}

#[wasm_bindgen_test]
fn mint_key_blob_inner_roundtrip() {
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth_env, auth_bytes) = signed_auth(&author, &device, vec![Capability::ManageSubscribers]);

    let subscribers: Vec<ActorKeypair> = (0..2).map(|_| ActorKeypair::generate()).collect();
    let period_key = [0x44u8; 32];

    let (envelope, bytes) = mint_key_blob_inner(
        &device.signing_key().to_bytes(),
        &auth_env,
        &auth_bytes,
        "Followers",
        3_000_000,
        &roster(&subscribers),
        &[],
        &period_key,
    )
    .expect("mint inner");

    // Replay the nest consumer path on the minted key_blob.
    let (blob_bytes, blob_env) = EmbedAsBytes {
        envelope,
        bytes,
        signer_auth: None,
    }
    .into_signed()
    .expect("minted envelope splits");
    let blob: KeyBlob = decode_signed_bytes(&blob_bytes).expect("decode KeyBlob (dag-cbor)");

    let (a_bytes, a_env) = EmbedAsBytes {
        envelope: auth_env,
        bytes: auth_bytes,
        signer_auth: None,
    }
    .into_signed()
    .expect("auth envelope splits");
    let device_auth: DeviceAuthorization =
        decode_signed_bytes(&a_bytes).expect("decode DeviceAuthorization");

    assert_eq!(blob.author, author.actor_id());
    assert_eq!(blob.signer, device.actor_id().0);
    assert_eq!(blob.tier, "Followers");
    assert_eq!(blob.entries.len(), 2);
    assert!(
        verify_key_blob_signature(
            &blob,
            &blob_bytes,
            &blob_env,
            &device_auth,
            &a_bytes,
            &a_env
        )
        .expect("verify chain runs"),
        "minted blob must verify under the same auth the nest checks",
    );
    for (i, sub) in subscribers.iter().enumerate() {
        let recovered = decrypt_key_blob_entry(sub, &blob.entries[i].encrypted_key).unwrap();
        assert_eq!(recovered, period_key);
    }
}

#[wasm_bindgen_test]
fn mint_key_blob_inner_hybrid_mixed_roster() {
    // S4b: the web app's `1184·N` flat-bytes ek marshalling + the all-zero
    // sentinel mint a per-entry suite — X-Wing for the subscriber whose slot
    // carries a published ek, classical for the zero-filled slot — and the
    // unified read dispatcher opens both.
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth_env, auth_bytes) = signed_auth(&author, &device, vec![Capability::ManageSubscribers]);

    let pq_sub = ActorKeypair::generate();
    let classical_sub = ActorKeypair::generate();
    let subs = [&pq_sub, &classical_sub];
    let period_key = [0x66u8; 32];

    // Slot 0 = pq_sub's real ek; slot 1 = all-zero (no ek).
    let mut eks = Vec::with_capacity(2 * 1184);
    eks.extend_from_slice(&subscriber_mlkem_encaps_key(&pq_sub));
    eks.extend_from_slice(&[0u8; 1184]);

    let (envelope, bytes) = mint_key_blob_inner(
        &device.signing_key().to_bytes(),
        &auth_env,
        &auth_bytes,
        "Pro",
        4_000_000,
        &roster(subs),
        &eks,
        &period_key,
    )
    .expect("hybrid mint inner");

    let (blob_bytes, _blob_env) = EmbedAsBytes {
        envelope,
        bytes,
        signer_auth: None,
    }
    .into_signed()
    .expect("minted envelope splits");
    let blob: KeyBlob = decode_signed_bytes(&blob_bytes).expect("decode KeyBlob");
    assert_eq!(blob.entries[0].suite, KemSuiteId::Xwing);
    assert_eq!(blob.entries[1].suite, KemSuiteId::Classical);
    assert_eq!(
        decrypt_key_blob_entry_for(&pq_sub, &blob.entries[0]).unwrap(),
        period_key
    );
    assert_eq!(
        decrypt_key_blob_entry_for(&classical_sub, &blob.entries[1]).unwrap(),
        period_key
    );
}

#[wasm_bindgen_test]
fn mint_key_blob_inner_rejects_missing_capability() {
    let author = ActorKeypair::generate();
    let (auth_env, auth_bytes) = signed_auth(&author, &author, vec![Capability::Post]);
    let result = mint_key_blob_inner(
        &author.signing_key().to_bytes(),
        &auth_env,
        &auth_bytes,
        "Pro",
        1,
        &author.actor_id().0,
        &[],
        &[0x55u8; 32],
    );
    assert!(result.is_err(), "missing capability must error");
}

#[wasm_bindgen_test]
fn mint_key_blob_inner_rejects_bad_lengths() {
    let author = ActorKeypair::generate();
    let (auth_env, auth_bytes) = signed_auth(&author, &author, vec![Capability::ManageSubscribers]);

    // Roster not a multiple of 32.
    assert!(
        mint_key_blob_inner(
            &author.signing_key().to_bytes(),
            &auth_env,
            &auth_bytes,
            "Pro",
            1,
            &[0u8; 31],
            &[],
            &[0u8; 32],
        )
        .is_err(),
        "malformed roster must error"
    );

    // wrapped_key not 32 bytes.
    assert!(
        mint_key_blob_inner(
            &author.signing_key().to_bytes(),
            &auth_env,
            &auth_bytes,
            "Pro",
            1,
            &author.actor_id().0,
            &[],
            &[0u8; 16],
        )
        .is_err(),
        "short wrapped_key must error"
    );

    // signer_secret not 32 bytes.
    assert!(
        mint_key_blob_inner(
            &[0u8; 16],
            &auth_env,
            &auth_bytes,
            "Pro",
            1,
            &author.actor_id().0,
            &[],
            &[0u8; 32],
        )
        .is_err(),
        "short signer_secret must error"
    );
}
