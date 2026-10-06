use fauna_ffi::*;

#[test]
fn generate_keypair_returns_32_bytes() {
    let secret = generate_keypair();
    assert_eq!(secret.len(), 32);
}

#[test]
fn actor_id_from_secret_roundtrip() {
    let secret = generate_keypair();
    let actor_id = actor_id_from_secret(secret.clone()).unwrap();
    assert_eq!(actor_id.len(), 32);
    // Same secret always yields same actor_id
    let actor_id2 = actor_id_from_secret(secret).unwrap();
    assert_eq!(actor_id, actor_id2);
}

#[test]
fn actor_id_from_secret_rejects_wrong_length() {
    let result = actor_id_from_secret(vec![0u8; 16]);
    assert!(result.is_err());
}

// `build_auth_request` left the FFI with the login nest binding (2026-09-23):
// a handshake is signed inside the launch machine, on the connection whose
// nest it names, never as a detached record — so its record test left too.

#[test]
fn build_register_request_returns_typed_record() {
    let secret = generate_keypair();
    let reg =
        build_register_request(secret.clone(), "alice".into(), "fauna.social".into()).unwrap();
    assert_eq!(reg.actor_id.len(), 32);
    assert_eq!(reg.handle, "alice");
    assert!(reg.timestamp > 0);
    assert_eq!(reg.signature.len(), 64);
}
