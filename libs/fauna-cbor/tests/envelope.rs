use ed25519_dalek::SigningKey;
use fauna_cbor::{SignedEnvelope, VerifyError};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Post {
    author: String,
    body: String,
}

fn fixed_key() -> SigningKey {
    let seed: [u8; 32] = [7u8; 32];
    SigningKey::from_bytes(&seed)
}

#[test]
fn sign_then_verify_round_trip() {
    let sk = fixed_key();
    let pk = sk.verifying_key();
    let post = Post {
        author: "alice".to_string(),
        body: "hello".to_string(),
    };

    let (bytes, env) = SignedEnvelope::sign(&post, &sk).expect("sign");
    env.verify_permissive(&bytes, &pk).expect("verify");
}

#[test]
fn verify_rejects_tampered_bytes() {
    let sk = fixed_key();
    let pk = sk.verifying_key();
    let post = Post {
        author: "alice".to_string(),
        body: "hello".to_string(),
    };

    let (mut bytes, env) = SignedEnvelope::sign(&post, &sk).expect("sign");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    assert_eq!(
        env.verify_permissive(&bytes, &pk),
        Err(VerifyError::CidMismatch)
    );
}

#[test]
fn verify_rejects_tampered_signature() {
    let sk = fixed_key();
    let pk = sk.verifying_key();
    let post = Post {
        author: "alice".to_string(),
        body: "hello".to_string(),
    };

    let (bytes, mut env) = SignedEnvelope::sign(&post, &sk).expect("sign");
    env.sig_mut()[0] ^= 0xff;
    assert_eq!(
        env.verify_permissive(&bytes, &pk),
        Err(VerifyError::SignatureInvalid)
    );
}

#[test]
fn verify_rejects_wrong_pubkey() {
    let sk = fixed_key();
    let other_pk = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
    let post = Post {
        author: "alice".to_string(),
        body: "hello".to_string(),
    };

    let (bytes, env) = SignedEnvelope::sign(&post, &sk).expect("sign");
    assert_eq!(
        env.verify_permissive(&bytes, &other_pk),
        Err(VerifyError::SignatureInvalid)
    );
}

#[test]
fn cid_matches_bytes_after_sign() {
    let sk = fixed_key();
    let post = Post {
        author: "bob".to_string(),
        body: "x".to_string(),
    };
    let (bytes, env) = SignedEnvelope::sign(&post, &sk).expect("sign");
    assert!(env.cid().matches(&bytes));
}
