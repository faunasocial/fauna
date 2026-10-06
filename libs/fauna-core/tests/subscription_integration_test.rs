//! End-to-end test: author creates a gated post, subscriber decrypts it.

use fauna_core::data::{ContentHash, Post, PostBody, Timestamp};
use fauna_core::identity::ActorKeypair;
use fauna_core::subscription::crypto::{
    create_key_blob_entry, decrypt_content, decrypt_key_blob_entry, derive_post_key,
    encrypt_content,
};
use fauna_core::subscription::preview::auto_preview;
use fauna_core::subscription::types::{GatedInfo, KeyAccess, KeyBlob};

#[test]
fn full_gated_post_lifecycle() {
    // --- Setup ---
    let author = ActorKeypair::generate();
    let subscriber_a = ActorKeypair::generate();
    let subscriber_b = ActorKeypair::generate();
    let non_subscriber = ActorKeypair::generate();

    // Author's broadcast period key for the "Pro" tier
    let period_key: [u8; 32] = rand::random();

    // --- Author creates a gated post ---

    // 1. Full content
    let full_content = PostBody::Text {
        content: "This is the full premium article with 500+ words of content. ".repeat(10),
        facets: vec![],
    };

    // 2. Auto-generate preview
    let preview = auto_preview(&full_content);
    match &preview {
        PostBody::Text { content, .. } => {
            assert!(content.len() <= 283); // 280 + "..."
            assert!(content.ends_with("..."));
        }
        _ => panic!("expected Text preview"),
    }

    // 3. Serialize and encrypt the full content
    let full_content_bytes = serde_json::to_vec(&full_content).unwrap();
    let fake_post_id = ContentHash::from_digest_raw([99u8; 32]);
    let post_key = derive_post_key(&period_key, &fake_post_id);
    let encrypted_blob = encrypt_content(&post_key, &full_content_bytes);
    let encrypted_ref = ContentHash::from_digest_raw({
        let hash = blake3::hash(&encrypted_blob);
        *hash.as_bytes()
    });

    // 4. Create key blob for subscribers
    let entry_a = create_key_blob_entry(&subscriber_a.actor_id(), &period_key);
    let entry_b = create_key_blob_entry(&subscriber_b.actor_id(), &period_key);
    let key_blob = KeyBlob {
        author: author.actor_id(),
        tier: "Pro".to_string(),
        rotated_at: Timestamp(1000000),
        entries: vec![entry_a, entry_b],
        signer: [0u8; 32],
        key_commitment: [0x6b; 32],
    };
    let key_blob_bytes =
        fauna_core::encoding::canonical_encode(&key_blob).expect("encode key blob");
    let key_blob_ref = ContentHash::from_digest_raw({
        let hash = blake3::hash(&key_blob_bytes);
        *hash.as_bytes()
    });

    // 5. Build the Post (sign-over-CID: envelope ships alongside, not embedded)
    let post = Post {
        author: author.actor_id(),
        created_at: Timestamp(2000000),
        body: preview,
        references: vec![],
        expires_at: None,
        gated: Some(GatedInfo {
            encrypted_ref,
            key_access: KeyAccess::Broadcast { key_blob_ref },
            tier: "Pro".to_string(),
            tier_rank: 2,
            seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
            attachment_refs: vec![],
        }),
        content_warning: None,
        origin: None,
    };

    // --- Subscriber A decrypts ---

    // 1. Find my entry in the key blob
    let my_entry = key_blob
        .entries
        .iter()
        .find(|e| e.subscriber == subscriber_a.actor_id())
        .expect("subscriber A should be in key blob");

    // 2. Decrypt period key
    let recovered_period_key =
        decrypt_key_blob_entry(&subscriber_a, &my_entry.encrypted_key).unwrap();
    assert_eq!(recovered_period_key, period_key);

    // 3. Derive post key
    let recovered_post_key = derive_post_key(&recovered_period_key, &fake_post_id);
    assert_eq!(recovered_post_key, post_key);

    // 4. Decrypt content
    let decrypted_bytes = decrypt_content(&recovered_post_key, &encrypted_blob).unwrap();
    let decrypted_body: PostBody = serde_json::from_slice(&decrypted_bytes).unwrap();

    match decrypted_body {
        PostBody::Text { content, .. } => {
            assert!(content.starts_with("This is the full premium article"));
        }
        _ => panic!("expected Text"),
    }

    // --- Non-subscriber cannot decrypt ---

    // Non-subscriber is not in the key blob
    let not_found = key_blob
        .entries
        .iter()
        .find(|e| e.subscriber == non_subscriber.actor_id());
    assert!(not_found.is_none());

    // Even if they try another subscriber's entry, it fails
    let wrong_result = decrypt_key_blob_entry(&non_subscriber, &my_entry.encrypted_key);
    assert!(wrong_result.is_err());

    // --- Post serialization roundtrip ---
    let post_json = serde_json::to_string(&post).unwrap();
    let deserialized: Post = serde_json::from_str(&post_json).unwrap();
    assert!(deserialized.gated.is_some());
    assert_eq!(deserialized.gated.unwrap().tier_rank, 2);
}
