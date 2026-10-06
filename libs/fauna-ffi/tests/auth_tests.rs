use ed25519_dalek::{Signature, Verifier};
use fauna_ffi::*;

#[test]
fn sign_message_signature_verifies() {
    let secret = generate_keypair();
    let actor_id = actor_id_from_secret(secret.clone()).unwrap();
    let msg = b"hello world".to_vec();

    let sig_bytes = sign_message(secret, msg.clone()).unwrap();
    assert_eq!(sig_bytes.len(), 64);

    let verifying_key =
        ed25519_dalek::VerifyingKey::from_bytes(&actor_id.try_into().unwrap()).unwrap();
    let sig = Signature::from_slice(&sig_bytes).unwrap();
    assert!(verifying_key.verify(&msg, &sig).is_ok());
}

#[test]
fn sign_message_rejects_wrong_secret_length() {
    let result = sign_message(vec![0u8; 16], b"hello".to_vec());
    assert!(result.is_err());
}

/// Cross-language fixture: this signature is pinned and the C# test at
/// `apps/fauna-windows/FaunaApp/FaunaApp.Tests/CryptoServiceTests.cs`
/// asserts against the same (secret, msg, expected_signature) tuple.
/// Ed25519 (RFC 8032) is deterministic, so any drift in either runtime
/// will trip one or both tests.
#[test]
fn sign_message_pinned_fixture() {
    let secret = vec![42u8; 32];
    let msg = b"fauna sign-message fixture v1".to_vec();
    let sig = sign_message(secret, msg).unwrap();
    let sig_hex = hex::encode(&sig);
    assert_eq!(
        sig_hex,
        "2ae13626f4e09d60064c7b95e439236c4e30520bf39a3bb3d420d71c9e38e09e454b5993333044178fffa8282c2ddc761519752a15bc9aaf31c56e075452c006",
        "Ed25519 signature drift — update the matching FaunaApp.Tests fixture too",
    );
}
