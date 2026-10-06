use fauna_core::data::{Capability, ContentHash, DeviceAuthorization, Timestamp};
use fauna_core::encoding::{
    EmbedAsBytes, canonical_decode, canonical_encode, decode_signed_bytes, sign_envelope,
    verify_envelope,
};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::subscription::crypto::{
    MintError, build_manage_subscribers_self_delegation, create_key_blob_entry, decrypt_content,
    decrypt_key_blob_entry, decrypt_key_blob_entry_for, derive_post_key, derive_web_render_key,
    encrypt_content, mint_key_blob, mint_key_blob_from_bytes, period_key_commitment,
    subscriber_mlkem_encaps_key, verify_key_blob_signature,
};
use fauna_core::subscription::types::{KemSuiteId, KeyBlob};

/// Build a self-signed `DeviceAuthorization` and return it together with
/// the embed-as-bytes wire pair `(bytes, envelope)`.
fn signed_auth(
    signer: &ActorKeypair,
    auth: DeviceAuthorization,
) -> (DeviceAuthorization, Vec<u8>, fauna_cbor::SignedEnvelope) {
    let (bytes, env) = sign_envelope(signer, &auth).expect("sign auth");
    (auth, bytes, env)
}

#[test]
fn x25519_key_conversion_roundtrip() {
    let keypair = ActorKeypair::generate();

    let x25519_secret = keypair.to_x25519_secret();
    let x25519_public = keypair.actor_id().to_x25519_public();

    let derived_public = x25519_dalek::PublicKey::from(&x25519_secret);
    assert_eq!(derived_public.as_bytes(), x25519_public.as_bytes());
}

#[test]
fn x25519_diffie_hellman() {
    let alice = ActorKeypair::generate();
    let bob = ActorKeypair::generate();

    let alice_secret = alice.to_x25519_secret();
    let bob_secret = bob.to_x25519_secret();
    let alice_public = alice.actor_id().to_x25519_public();
    let bob_public = bob.actor_id().to_x25519_public();

    let alice_shared = alice_secret.diffie_hellman(&bob_public);
    let bob_shared = bob_secret.diffie_hellman(&alice_public);

    assert_eq!(alice_shared.as_bytes(), bob_shared.as_bytes());
}

#[test]
fn derive_post_key_deterministic() {
    let base_key = [42u8; 32];
    let post_id = ContentHash::from_digest_raw([1u8; 32]);

    let k1 = derive_post_key(&base_key, &post_id);
    let k2 = derive_post_key(&base_key, &post_id);

    assert_eq!(k1, k2);
}

#[test]
fn different_posts_different_keys() {
    let base_key = [42u8; 32];
    let post_id_a = ContentHash::from_digest_raw([1u8; 32]);
    let post_id_b = ContentHash::from_digest_raw([2u8; 32]);

    let k_a = derive_post_key(&base_key, &post_id_a);
    let k_b = derive_post_key(&base_key, &post_id_b);

    assert_ne!(k_a, k_b);
}

#[test]
fn derive_post_key_is_blake3_keyed_hash_under_context_subkey() {
    // Pin the algorithm: derive_key("fauna.gated.v1", base_key) for context
    // separation, then keyed_hash(intermediate, post_id) for the per-post salt.
    let base_key = [42u8; 32];
    let post_id = ContentHash::from_digest_raw([1u8; 32]);

    let intermediate = blake3::derive_key("fauna.gated.v1", &base_key);
    let expected = *blake3::keyed_hash(&intermediate, &post_id.digest()).as_bytes();

    assert_eq!(derive_post_key(&base_key, &post_id), expected);
}

#[test]
fn post_body_and_web_render_keys_never_coincide() {
    // The two seals must never coincide for the same post under the same
    // period key: the box holds the web-render key to serve paywalled HTML,
    // and a shared key would widen that into the gated body capability.
    let base_key = [42u8; 32];
    let post_id = ContentHash::from_digest_raw([1u8; 32]);

    assert_ne!(
        derive_post_key(&base_key, &post_id),
        derive_web_render_key(&base_key, &post_id),
    );
}

#[test]
fn encrypt_content_emits_chacha20_poly1305_envelope() {
    // Pin the AEAD: encrypt_content's output must decrypt with
    // ChaCha20-Poly1305 directly given the same key + the leading 12-byte
    // nonce. AES-GCM-shaped output would fail this check.
    use chacha20poly1305::{
        ChaCha20Poly1305, Nonce,
        aead::{Aead, KeyInit},
    };

    let key = [99u8; 32];
    let plaintext = b"This is the full gated post content.";

    let envelope = encrypt_content(&key, plaintext);
    assert!(envelope.len() >= 12 + 16, "nonce + tag floor");
    let (nonce_bytes, ct) = envelope.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);
    let cipher = ChaCha20Poly1305::new((&key).into());
    let recovered = cipher
        .decrypt(nonce, ct)
        .expect("ChaCha20-Poly1305 must decrypt the envelope");
    assert_eq!(&recovered[..], plaintext);
}

#[test]
fn key_blob_entry_uses_chacha20_poly1305() {
    // Pin the KeyBlob-entry AEAD by re-deriving the ECDH wrap key the way
    // create_key_blob_entry does and decrypting with ChaCha20-Poly1305
    // directly. AES-GCM output would fail.
    use chacha20poly1305::{
        ChaCha20Poly1305, Nonce,
        aead::{Aead, KeyInit},
    };
    use x25519_dalek::PublicKey as X25519PublicKey;

    let subscriber = ActorKeypair::generate();
    let period_key = [77u8; 32];

    let entry = create_key_blob_entry(&subscriber.actor_id(), &period_key);

    // Layout: 32 (ephemeral pubkey) || 12 (nonce) || ct+tag
    assert!(entry.encrypted_key.len() >= 32 + 12 + 16);
    let mut eph_bytes = [0u8; 32];
    eph_bytes.copy_from_slice(&entry.encrypted_key[..32]);
    let ephemeral_public = X25519PublicKey::from(eph_bytes);
    let nonce = Nonce::from_slice(&entry.encrypted_key[32..44]);
    let ciphertext = &entry.encrypted_key[44..];

    // Subscriber-side ECDH → BLAKE3 derive_key for the wrap key.
    let subscriber_secret = subscriber.to_x25519_secret();
    let shared = subscriber_secret.diffie_hellman(&ephemeral_public);
    let wrap_key = blake3::derive_key("fauna.keyblob.v1", shared.as_bytes());

    let cipher = ChaCha20Poly1305::new((&wrap_key).into());
    let recovered = cipher
        .decrypt(nonce, ciphertext)
        .expect("ChaCha20-Poly1305 must decrypt the KeyBlob entry");
    assert_eq!(&recovered[..], &period_key[..]);
}

#[test]
fn encrypt_decrypt_roundtrip() {
    let key = [99u8; 32];
    let plaintext = b"This is the full gated post content.";

    let ciphertext = encrypt_content(&key, plaintext);
    let decrypted = decrypt_content(&key, &ciphertext).unwrap();

    assert_eq!(decrypted, plaintext);
}

#[test]
fn decrypt_with_wrong_key_fails() {
    let key = [99u8; 32];
    let wrong_key = [100u8; 32];
    let plaintext = b"Secret content";

    let ciphertext = encrypt_content(&key, plaintext);
    let result = decrypt_content(&wrong_key, &ciphertext);

    assert!(result.is_err());
}

#[test]
fn encrypt_produces_different_ciphertext_each_time() {
    let key = [99u8; 32];
    let plaintext = b"Same content";

    let ct1 = encrypt_content(&key, plaintext);
    let ct2 = encrypt_content(&key, plaintext);

    assert_ne!(ct1, ct2);
}

#[test]
fn key_blob_entry_roundtrip() {
    let subscriber = ActorKeypair::generate();
    let period_key = [77u8; 32];

    let entry = create_key_blob_entry(&subscriber.actor_id(), &period_key);

    let decrypted = decrypt_key_blob_entry(&subscriber, &entry.encrypted_key).unwrap();

    assert_eq!(decrypted, period_key);
}

#[test]
fn key_blob_entry_wrong_subscriber_fails() {
    let subscriber = ActorKeypair::generate();
    let wrong_subscriber = ActorKeypair::generate();
    let period_key = [77u8; 32];

    let entry = create_key_blob_entry(&subscriber.actor_id(), &period_key);

    let result = decrypt_key_blob_entry(&wrong_subscriber, &entry.encrypted_key);

    assert!(result.is_err());
}

#[test]
fn key_blob_multiple_subscribers() {
    let period_key = [55u8; 32];
    let subscribers: Vec<ActorKeypair> = (0..5).map(|_| ActorKeypair::generate()).collect();

    let entries: Vec<_> = subscribers
        .iter()
        .map(|s| create_key_blob_entry(&s.actor_id(), &period_key))
        .collect();

    // Each subscriber can decrypt their own entry
    for (i, sub) in subscribers.iter().enumerate() {
        let decrypted = decrypt_key_blob_entry(sub, &entries[i].encrypted_key).unwrap();
        assert_eq!(decrypted, period_key);
    }

    // But not another subscriber's entry
    let result = decrypt_key_blob_entry(&subscribers[0], &entries[1].encrypted_key);
    assert!(result.is_err());
}

#[test]
fn key_blob_signing_and_verification() {
    let author = ActorKeypair::generate();
    let nest = ActorKeypair::generate();

    let device_auth = DeviceAuthorization {
        actor_id: author.actor_id(),
        device_key: nest.actor_id().0,
        capabilities: vec![Capability::ManageSubscribers],
        created_at: Timestamp(1000000),
        expires_at: None,
    };
    let (auth_bytes, auth_env) = sign_envelope(&author, &device_auth).unwrap();
    verify_envelope(&device_auth, &auth_bytes, &auth_env).unwrap();

    let key_blob = KeyBlob {
        author: author.actor_id(),
        tier: "Pro".to_string(),
        rotated_at: Timestamp(2000000),
        entries: vec![],
        signer: nest.actor_id().0,
        key_commitment: [0x6b; 32],
    };
    let (blob_bytes, blob_env) = sign_envelope(&nest, &key_blob).unwrap();
    verify_envelope(&key_blob, &blob_bytes, &blob_env).unwrap();

    assert!(
        verify_key_blob_signature(
            &key_blob,
            &blob_bytes,
            &blob_env,
            &device_auth,
            &auth_bytes,
            &auth_env,
        )
        .unwrap()
    );
}

#[test]
fn key_blob_verification_fails_wrong_signer() {
    let author = ActorKeypair::generate();
    let nest = ActorKeypair::generate();
    let imposter = ActorKeypair::generate();

    let device_auth = DeviceAuthorization {
        actor_id: author.actor_id(),
        device_key: nest.actor_id().0,
        capabilities: vec![Capability::ManageSubscribers],
        created_at: Timestamp(1000000),
        expires_at: None,
    };
    let (auth_bytes, auth_env) = sign_envelope(&author, &device_auth).unwrap();

    let key_blob = KeyBlob {
        author: author.actor_id(),
        tier: "Pro".to_string(),
        rotated_at: Timestamp(2000000),
        entries: vec![],
        signer: imposter.actor_id().0,
        key_commitment: [0x6b; 32],
    };
    let (blob_bytes, blob_env) = sign_envelope(&imposter, &key_blob).unwrap();

    assert!(
        !verify_key_blob_signature(
            &key_blob,
            &blob_bytes,
            &blob_env,
            &device_auth,
            &auth_bytes,
            &auth_env,
        )
        .unwrap()
    );
}

#[test]
fn key_blob_verification_fails_missing_capability() {
    let author = ActorKeypair::generate();
    let nest = ActorKeypair::generate();

    let device_auth = DeviceAuthorization {
        actor_id: author.actor_id(),
        device_key: nest.actor_id().0,
        capabilities: vec![Capability::Post],
        created_at: Timestamp(1000000),
        expires_at: None,
    };
    let (auth_bytes, auth_env) = sign_envelope(&author, &device_auth).unwrap();

    let key_blob = KeyBlob {
        author: author.actor_id(),
        tier: "Pro".to_string(),
        rotated_at: Timestamp(2000000),
        entries: vec![],
        signer: nest.actor_id().0,
        key_commitment: [0x6b; 32],
    };
    let (blob_bytes, blob_env) = sign_envelope(&nest, &key_blob).unwrap();

    assert!(
        !verify_key_blob_signature(
            &key_blob,
            &blob_bytes,
            &blob_env,
            &device_auth,
            &auth_bytes,
            &auth_env,
        )
        .unwrap()
    );
}

// ── Author-client KeyBlob mint (encrypted-mode broadcast path) ──────────

fn make_signer_auth(
    author: &ActorKeypair,
    signer: &ActorKeypair,
) -> (DeviceAuthorization, Vec<u8>, fauna_cbor::SignedEnvelope) {
    let auth = DeviceAuthorization {
        actor_id: author.actor_id(),
        device_key: signer.actor_id().0,
        capabilities: vec![Capability::ManageSubscribers],
        created_at: Timestamp(1000000),
        expires_at: None,
    };
    signed_auth(author, auth)
}

#[test]
fn mint_key_blob_roundtrip_with_subscribers() {
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth, auth_bytes, auth_env) = make_signer_auth(&author, &device);

    let subscribers: Vec<ActorKeypair> = (0..3).map(|_| ActorKeypair::generate()).collect();
    let subscriber_ids: Vec<_> = subscribers.iter().map(|s| s.actor_id()).collect();
    let period_key = [33u8; 32];

    let minted = mint_key_blob(
        &device,
        &auth,
        &auth_bytes,
        &auth_env,
        "Pro".to_string(),
        Timestamp(2_000_000),
        &subscriber_ids,
        &[],
        &period_key,
    )
    .expect("mint succeeds");

    assert_eq!(minted.blob.author, author.actor_id());
    assert_eq!(minted.blob.signer, device.actor_id().0);
    assert_eq!(minted.blob.tier, "Pro");
    assert_eq!(minted.blob.entries.len(), 3);
    assert!(
        verify_key_blob_signature(
            &minted.blob,
            &minted.bytes,
            &minted.envelope,
            &auth,
            &auth_bytes,
            &auth_env,
        )
        .unwrap()
    );

    for (i, sub) in subscribers.iter().enumerate() {
        let recovered = decrypt_key_blob_entry(sub, &minted.blob.entries[i].encrypted_key).unwrap();
        assert_eq!(recovered, period_key);
    }
}

#[test]
fn mint_key_blob_hybrid_when_eks_published() {
    // S4b: when a subscriber published an ML-KEM ek, that
    // subscriber's entry is X-Wing and the subscriber opens it via the hybrid
    // read dispatcher `decrypt_key_blob_entry_for`.
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth, auth_bytes, auth_env) = make_signer_auth(&author, &device);

    let subscribers: Vec<ActorKeypair> = (0..3).map(|_| ActorKeypair::generate()).collect();
    let subscriber_ids: Vec<_> = subscribers.iter().map(|s| s.actor_id()).collect();
    // Every subscriber published their identity-seed-derived ek.
    let eks: Vec<Option<[u8; 1184]>> = subscribers
        .iter()
        .map(|s| Some(subscriber_mlkem_encaps_key(s)))
        .collect();
    let period_key = [33u8; 32];

    let minted = mint_key_blob(
        &device,
        &auth,
        &auth_bytes,
        &auth_env,
        "Pro".to_string(),
        Timestamp(2_000_000),
        &subscriber_ids,
        &eks,
        &period_key,
    )
    .expect("hybrid mint succeeds");

    assert_eq!(minted.blob.entries.len(), 3);
    for (i, sub) in subscribers.iter().enumerate() {
        assert_eq!(
            minted.blob.entries[i].suite,
            KemSuiteId::Xwing,
            "entry {i} is X-Wing"
        );
        let recovered = decrypt_key_blob_entry_for(sub, &minted.blob.entries[i]).unwrap();
        assert_eq!(recovered, period_key);
    }
}

#[test]
fn mint_key_blob_mixed_roster_per_subscriber_suite() {
    // S4b: a mixed roster (subscriber 0 published an ek, subscriber 1 did not)
    // mints a per-entry suite — X-Wing for the publisher, classical for the
    // other — and the unified read dispatcher opens both.
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth, auth_bytes, auth_env) = make_signer_auth(&author, &device);

    let pq_sub = ActorKeypair::generate();
    let classical_sub = ActorKeypair::generate();
    let subscriber_ids = [pq_sub.actor_id(), classical_sub.actor_id()];
    let eks = [Some(subscriber_mlkem_encaps_key(&pq_sub)), None];
    let period_key = [44u8; 32];

    let minted = mint_key_blob(
        &device,
        &auth,
        &auth_bytes,
        &auth_env,
        "Pro".to_string(),
        Timestamp(2_000_000),
        &subscriber_ids,
        &eks,
        &period_key,
    )
    .expect("mixed mint succeeds");

    assert_eq!(minted.blob.entries[0].suite, KemSuiteId::Xwing);
    assert_eq!(minted.blob.entries[1].suite, KemSuiteId::Classical);
    assert_eq!(
        decrypt_key_blob_entry_for(&pq_sub, &minted.blob.entries[0]).unwrap(),
        period_key
    );
    assert_eq!(
        decrypt_key_blob_entry_for(&classical_sub, &minted.blob.entries[1]).unwrap(),
        period_key
    );
}

#[test]
fn mint_key_blob_self_signed_author_device() {
    // Author and signing device are the same keypair — valid self-delegation.
    let author = ActorKeypair::generate();
    let (auth, auth_bytes, auth_env) = make_signer_auth(&author, &author);

    let subscriber = ActorKeypair::generate();
    let period_key = [44u8; 32];

    let minted = mint_key_blob(
        &author,
        &auth,
        &auth_bytes,
        &auth_env,
        "Followers".to_string(),
        Timestamp(3_000_000),
        std::slice::from_ref(&subscriber.actor_id()),
        &[],
        &period_key,
    )
    .expect("self-signed mint succeeds");

    assert_eq!(minted.blob.author, author.actor_id());
    assert_eq!(minted.blob.signer, author.actor_id().0);
    assert!(
        verify_key_blob_signature(
            &minted.blob,
            &minted.bytes,
            &minted.envelope,
            &auth,
            &auth_bytes,
            &auth_env,
        )
        .unwrap()
    );

    let recovered =
        decrypt_key_blob_entry(&subscriber, &minted.blob.entries[0].encrypted_key).unwrap();
    assert_eq!(recovered, period_key);
}

#[test]
fn self_delegation_builder_mints_a_verifiable_keyblob() {
    // The production `build_manage_subscribers_self_delegation` must yield an
    // authorization the encrypted-mode mint+verify chain accepts end-to-end —
    // this is the second client-state piece the mint needs (the first being the
    // period key). The author is the single device.
    let author = ActorKeypair::generate();
    let signed = build_manage_subscribers_self_delegation(&author, Timestamp(1_000))
        .expect("self-delegation signs");

    // Shape: author is both actor and device, carries exactly ManageSubscribers.
    assert_eq!(signed.auth.actor_id, author.actor_id());
    assert_eq!(signed.auth.device_key, author.actor_id().0);
    assert!(matches!(
        signed.auth.capabilities.as_slice(),
        [Capability::ManageSubscribers]
    ));
    assert!(signed.auth.expires_at.is_none());
    // The signature self-verifies.
    assert!(verify_envelope(&signed.auth, &signed.bytes, &signed.envelope).is_ok());

    // And it drives a real mint that `verify_key_blob_signature` accepts.
    let subscriber = ActorKeypair::generate();
    let period_key = [0x55u8; 32];
    let minted = mint_key_blob(
        &author,
        &signed.auth,
        &signed.bytes,
        &signed.envelope,
        "gold".to_string(),
        Timestamp(2_000),
        std::slice::from_ref(&subscriber.actor_id()),
        &[],
        &period_key,
    )
    .expect("mint with self-delegation succeeds");
    assert!(
        verify_key_blob_signature(
            &minted.blob,
            &minted.bytes,
            &minted.envelope,
            &signed.auth,
            &signed.bytes,
            &signed.envelope,
        )
        .unwrap()
    );

    // The `wire()` form round-trips back to the same authorization (the shape
    // the `EncryptedKeyBlobUpload.signer_auth` field carries).
    let wire = signed.wire();
    let (rt_bytes, _rt_env) = wire.into_signed().expect("wire decodes");
    let rt_auth: DeviceAuthorization = decode_signed_bytes(&rt_bytes).expect("auth decodes");
    assert_eq!(rt_auth.actor_id, author.actor_id());
}

#[test]
fn mint_key_blob_rejects_wrong_signing_key() {
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let imposter = ActorKeypair::generate();
    let (auth, auth_bytes, auth_env) = make_signer_auth(&author, &device);

    // `imposter` doesn't match `auth.device_key` (which is `device`).
    let result = mint_key_blob(
        &imposter,
        &auth,
        &auth_bytes,
        &auth_env,
        "Pro".to_string(),
        Timestamp(2_000_000),
        &[ActorKeypair::generate().actor_id()],
        &[],
        &[55u8; 32],
    );
    assert_eq!(result.unwrap_err(), MintError::SignerMismatch);
}

#[test]
fn mint_key_blob_rejects_missing_capability() {
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth, auth_bytes, auth_env) = signed_auth(
        &author,
        DeviceAuthorization {
            actor_id: author.actor_id(),
            device_key: device.actor_id().0,
            capabilities: vec![Capability::Post], // No ManageSubscribers / All
            created_at: Timestamp(1000000),
            expires_at: None,
        },
    );

    let result = mint_key_blob(
        &device,
        &auth,
        &auth_bytes,
        &auth_env,
        "Pro".to_string(),
        Timestamp(2_000_000),
        &[ActorKeypair::generate().actor_id()],
        &[],
        &[55u8; 32],
    );
    assert_eq!(result.unwrap_err(), MintError::MissingCapability);
}

#[test]
fn mint_key_blob_rejects_unsigned_auth() {
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    // Auth signed by a wrong keypair — `verify_envelope` must reject before
    // minting, so the function returns `MintError::InvalidAuth`.
    let auth = DeviceAuthorization {
        actor_id: author.actor_id(),
        device_key: device.actor_id().0,
        capabilities: vec![Capability::ManageSubscribers],
        created_at: Timestamp(1000000),
        expires_at: None,
    };
    let imposter = ActorKeypair::generate();
    let (auth_bytes, auth_env) = sign_envelope(&imposter, &auth).unwrap();

    let result = mint_key_blob(
        &device,
        &auth,
        &auth_bytes,
        &auth_env,
        "Pro".to_string(),
        Timestamp(2_000_000),
        &[ActorKeypair::generate().actor_id()],
        &[],
        &[55u8; 32],
    );
    assert_eq!(result.unwrap_err(), MintError::InvalidAuth);
}

#[test]
fn mint_key_blob_archival_shape_matches_period_shape() {
    // The same mint primitive is used for the MLS-to-broadcast archival blob.
    // The differences are caller-supplied: tier name carries the archival
    // suffix, and `wrapped_key` is the final MLS epoch secret rather than a
    // random period key. Verification works identically.
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth, auth_bytes, auth_env) = make_signer_auth(&author, &device);

    let subscriber = ActorKeypair::generate();
    let epoch_secret = [88u8; 32];

    let minted = mint_key_blob(
        &device,
        &auth,
        &auth_bytes,
        &auth_env,
        "Pro:archive:epoch:42".to_string(),
        Timestamp(5_000_000),
        &[subscriber.actor_id()],
        &[],
        &epoch_secret,
    )
    .expect("archival mint succeeds");

    assert!(
        verify_key_blob_signature(
            &minted.blob,
            &minted.bytes,
            &minted.envelope,
            &auth,
            &auth_bytes,
            &auth_env,
        )
        .unwrap()
    );
    let recovered =
        decrypt_key_blob_entry(&subscriber, &minted.blob.entries[0].encrypted_key).unwrap();
    assert_eq!(recovered, epoch_secret);
}

#[test]
fn mint_key_blob_survives_serialization_roundtrip() {
    // Stand-in for the wire round-trip a real upload/fetch would take.
    // Storage format is the dag-cbor-encoded `EmbedAsBytes` wire shape
    // (envelope + canonical bytes), not raw KeyBlob bytes.
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth, auth_bytes, auth_env) = make_signer_auth(&author, &device);

    let subscriber = ActorKeypair::generate();
    let period_key = [11u8; 32];

    let minted = mint_key_blob(
        &device,
        &auth,
        &auth_bytes,
        &auth_env,
        "Pro".to_string(),
        Timestamp(2_000_000),
        &[subscriber.actor_id()],
        &[],
        &period_key,
    )
    .unwrap();

    let wire = EmbedAsBytes::from_signed(minted.bytes.clone(), minted.envelope);
    let stored = canonical_encode(&wire).unwrap();
    let restored_wire: EmbedAsBytes = canonical_decode(&stored).unwrap();
    let (restored_bytes, restored_env) = restored_wire.into_signed().unwrap();
    let restored: KeyBlob = decode_signed_bytes(&restored_bytes).unwrap();

    assert!(
        verify_key_blob_signature(
            &restored,
            &restored_bytes,
            &restored_env,
            &auth,
            &auth_bytes,
            &auth_env,
        )
        .unwrap()
    );
    let recovered =
        decrypt_key_blob_entry(&subscriber, &restored.entries[0].encrypted_key).unwrap();
    assert_eq!(recovered, period_key);
}

#[test]
fn mint_key_blob_rejects_tampered_entry() {
    // Tampering with an entry's encrypted_key must break unsealing (AEAD auth
    // catches it) even though the KeyBlob signature itself can be replaced.
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth, auth_bytes, auth_env) = make_signer_auth(&author, &device);

    let subscriber = ActorKeypair::generate();
    let period_key = [22u8; 32];

    let mut minted = mint_key_blob(
        &device,
        &auth,
        &auth_bytes,
        &auth_env,
        "Pro".to_string(),
        Timestamp(2_000_000),
        &[subscriber.actor_id()],
        &[],
        &period_key,
    )
    .unwrap();

    // Flip one byte inside the ciphertext (skip the 32-byte ephemeral pubkey
    // prefix so we hit AEAD-protected bytes).
    minted.blob.entries[0].encrypted_key[50] ^= 0x01;

    let result = decrypt_key_blob_entry(&subscriber, &minted.blob.entries[0].encrypted_key);
    assert!(result.is_err());
}

// ── Byte-marshalling mint wrapper (shared FFI/WASM author-side core) ────────

/// Build the embed-as-bytes `(envelope, bytes)` pair for a signed
/// `ManageSubscribers` self-delegation — the `signer_auth_*` args the FFI/WASM
/// bindings feed into `mint_key_blob_from_bytes`.
fn signer_auth_embed(author: &ActorKeypair, signer: &ActorKeypair) -> (Vec<u8>, Vec<u8>) {
    let (_auth, bytes, env) = make_signer_auth(author, signer);
    let wire = EmbedAsBytes::from_signed(bytes, env);
    (wire.envelope, wire.bytes)
}

#[test]
fn mint_key_blob_from_bytes_roundtrip() {
    // The shared byte-parse wrapper must produce the same nest-verifiable
    // key_blob embed the typed `mint_key_blob` does, given raw byte inputs.
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth_env, auth_bytes) = signer_auth_embed(&author, &device);

    let subscribers: Vec<ActorId> = (0..2)
        .map(|_| ActorKeypair::generate().actor_id())
        .collect();
    let period_key = [0x33u8; 32];

    let (envelope, bytes) = mint_key_blob_from_bytes(
        &device.signing_key().to_bytes(),
        &auth_env,
        &auth_bytes,
        "Pro".to_string(),
        2_000_000,
        &subscribers,
        &[],
        &period_key,
    )
    .expect("mint from bytes succeeds");

    // The returned pair is exactly the nest-verifiable key_blob embed.
    let (blob_bytes, blob_env) = EmbedAsBytes {
        envelope,
        bytes,
        signer_auth: None,
    }
    .into_signed()
    .expect("minted envelope splits");
    let blob: KeyBlob = decode_signed_bytes(&blob_bytes).expect("decode KeyBlob");
    assert_eq!(blob.author, author.actor_id());
    assert_eq!(blob.signer, device.actor_id().0);
    assert_eq!(blob.tier, "Pro");
    assert_eq!(blob.entries.len(), 2);

    let (a_bytes, a_env) = EmbedAsBytes {
        envelope: auth_env,
        bytes: auth_bytes,
        signer_auth: None,
    }
    .into_signed()
    .expect("auth envelope splits");
    let device_auth: DeviceAuthorization =
        decode_signed_bytes(&a_bytes).expect("decode DeviceAuthorization");
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
        "blob minted from raw bytes must verify under the same auth the nest checks",
    );
}

#[test]
fn mint_key_blob_from_bytes_rejects_short_signer_secret() {
    let author = ActorKeypair::generate();
    let (auth_env, auth_bytes) = signer_auth_embed(&author, &author);
    let err = mint_key_blob_from_bytes(
        &[0u8; 16],
        &auth_env,
        &auth_bytes,
        "Pro".to_string(),
        1,
        &[author.actor_id()],
        &[],
        &[0u8; 32],
    )
    .unwrap_err();
    assert!(err.contains("signer_secret must be 32 bytes"), "got: {err}");
}

#[test]
fn mint_key_blob_from_bytes_rejects_short_wrapped_key() {
    let author = ActorKeypair::generate();
    let (auth_env, auth_bytes) = signer_auth_embed(&author, &author);
    let err = mint_key_blob_from_bytes(
        &author.signing_key().to_bytes(),
        &auth_env,
        &auth_bytes,
        "Pro".to_string(),
        1,
        &[author.actor_id()],
        &[],
        &[0u8; 16],
    )
    .unwrap_err();
    assert!(err.contains("wrapped_key must be 32 bytes"), "got: {err}");
}

#[test]
fn mint_key_blob_from_bytes_rejects_malformed_signer_auth() {
    // An envelope/bytes pair that isn't a valid embed-as-bytes triple must fail
    // the parse before reaching the typed core mint.
    let author = ActorKeypair::generate();
    let err = mint_key_blob_from_bytes(
        &author.signing_key().to_bytes(),
        &[0u8; 8],
        &[0u8; 8],
        "Pro".to_string(),
        1,
        &[author.actor_id()],
        &[],
        &[0u8; 32],
    )
    .unwrap_err();
    assert!(
        err.contains("signer_auth envelope") || err.contains("decode signer_auth"),
        "got: {err}"
    );
}

#[test]
fn mint_key_blob_embed_as_bytes_storage_round_trip_preserves_signature() {
    // Asserts that the dag-cbor-encoded `EmbedAsBytes` wire shape (the format the
    // nest persists in `current_key_blobs.blob_data`) round-trips losslessly
    // and that `verify_key_blob_signature` still passes on the restored
    // values — the invariant the encrypted-mode upload path relies on.
    let author = ActorKeypair::generate();
    let subs: Vec<ActorId> = (0..2)
        .map(|_| ActorKeypair::generate().actor_id())
        .collect();
    let period_key = [0x77u8; 32];

    let (auth, auth_bytes, auth_env) = signed_auth(
        &author,
        DeviceAuthorization {
            actor_id: author.actor_id(),
            device_key: author.actor_id().0,
            capabilities: vec![Capability::ManageSubscribers],
            created_at: Timestamp(1_700_000_000_000_000),
            expires_at: None,
        },
    );

    let minted = mint_key_blob(
        &author,
        &auth,
        &auth_bytes,
        &auth_env,
        "tierA".to_string(),
        Timestamp(1_700_000_500_000_000),
        &subs,
        &[],
        &period_key,
    )
    .unwrap();

    let wire = EmbedAsBytes::from_signed(minted.bytes.clone(), minted.envelope);
    let stored = canonical_encode(&wire).unwrap();
    let restored_wire: EmbedAsBytes = canonical_decode(&stored).unwrap();
    let (restored_bytes, restored_env) = restored_wire.into_signed().unwrap();
    let restored: KeyBlob = decode_signed_bytes(&restored_bytes).unwrap();

    // All non-signature fields survive the round-trip.
    assert_eq!(restored.author, minted.blob.author);
    assert_eq!(restored.tier, minted.blob.tier);
    assert_eq!(restored.rotated_at, minted.blob.rotated_at);
    assert_eq!(restored.signer, minted.blob.signer);
    assert_eq!(restored.entries.len(), minted.blob.entries.len());

    // The delegation chain still verifies on the restored blob.
    assert!(
        verify_key_blob_signature(
            &restored,
            &restored_bytes,
            &restored_env,
            &auth,
            &auth_bytes,
            &auth_env,
        )
        .unwrap()
    );
}

// ── the key witness (`KeyBlob::key_commitment`) ──────────────────────────

#[test]
/// A minted blob commits to the key its entries wrap — the same commitment
/// whoever mints it, over whatever roster, at whatever stamp — and the
/// commitment survives the sign-over-CID round trip. That is what lets the
/// author ask a stored blob which key it wraps without holding an entry.
fn a_minted_blob_commits_to_the_key_it_wraps() {
    let author = ActorKeypair::generate();
    let device = ActorKeypair::generate();
    let (auth, auth_bytes, auth_env) = make_signer_auth(&author, &device);
    let period_key = [0x33u8; 32];
    let expected = period_key_commitment(&period_key);
    assert_eq!(
        expected,
        blake3::derive_key("fauna.keyblob.commit.v1", &period_key),
        "the ratified context string"
    );

    let mint = |rotated_at: u64, subscribers: &[ActorId], key: &[u8; 32]| {
        mint_key_blob(
            &device,
            &auth,
            &auth_bytes,
            &auth_env,
            "Pro".to_string(),
            Timestamp(rotated_at),
            subscribers,
            &[],
            key,
        )
        .expect("mint")
    };
    let one = mint(
        2_000_000,
        &[ActorKeypair::generate().actor_id()],
        &period_key,
    );
    assert_eq!(one.blob.key_commitment, expected);
    let decoded: KeyBlob = decode_signed_bytes(&one.bytes).expect("decode minted bytes");
    assert_eq!(decoded.key_commitment, expected, "survives the wire");

    let two = mint(9_000_000, &[], &period_key);
    assert_eq!(
        two.blob.key_commitment, expected,
        "roster and stamp do not enter"
    );
    let other = mint(2_000_000, &[], &[0x34u8; 32]);
    assert_ne!(
        other.blob.key_commitment, expected,
        "a different key commits differently"
    );

    // One-way and domain-separated: the commitment is none of the keys the
    // period key derives, and not the key itself.
    let post = ContentHash::from_digest_raw([9u8; 32]);
    assert_ne!(expected, derive_post_key(&period_key, &post));
    assert_ne!(expected, derive_web_render_key(&period_key, &post));
    assert_ne!(expected, period_key);
}

#[test]
/// The witness rides the signed blob under its own key, and a blob without
/// it is refused at decode — no reader can be handed the weaker freshness
/// rule by a blob that simply omits the field.
fn the_witness_is_signed_and_a_blob_without_one_is_refused() {
    let author = ActorKeypair::generate();
    let witnessed = KeyBlob {
        author: author.actor_id(),
        tier: "Pro".to_string(),
        rotated_at: Timestamp(2_000_000),
        entries: vec![],
        signer: author.actor_id().0,
        key_commitment: period_key_commitment(&[0x33u8; 32]),
    };
    let witnessed_bytes = canonical_encode(&witnessed).expect("encode");
    assert!(
        witnessed_bytes.windows(14).any(|w| w == b"key_commitment"),
        "the witness is written under its own key"
    );
    let (bytes, env) = sign_envelope(&author, &witnessed).expect("sign");
    let round: KeyBlob = decode_signed_bytes(&bytes).expect("decode signed");
    assert_eq!(round.key_commitment, witnessed.key_commitment);
    verify_envelope(&round, &bytes, &env).expect("verifies");

    let mut map: std::collections::BTreeMap<String, fauna_cbor::Value> =
        canonical_decode(&witnessed_bytes).expect("decode as a map");
    map.remove("key_commitment");
    let witnessless = canonical_encode(&map).expect("encode");
    assert!(canonical_decode::<KeyBlob>(&witnessless).is_err());
}
