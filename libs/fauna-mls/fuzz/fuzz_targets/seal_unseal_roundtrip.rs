#![no_main]

//! Feed arbitrary inputs through seal_wrapped_msek, then unseal with
//! the same credential. Round-trip must succeed; any panic is a bug.

use fauna_mls::wrapped_blob::{
    seal_wrapped_msek, unseal_wrapped_msek, Argon2idParams, CredentialInput, KdfParams,
};
use libfuzzer_sys::fuzz_target;

#[derive(arbitrary::Arbitrary, Debug)]
struct Input<'a> {
    msek: [u8; 32],
    actor: [u8; 32],
    credential_id: &'a str,
    password: &'a [u8],
}

fuzz_target!(|input: Input<'_>| {
    // Skip empty credential_id (spec requires it; ASCII-safe slicing
    // also avoids panicking on bad UTF-8 — `&str` already enforces it).
    if input.credential_id.is_empty() {
        return;
    }
    let cred = CredentialInput::Plain(input.password);
    let p = KdfParams::Argon2id(Argon2idParams { m: 4096, t: 1, p: 1 });
    let blob = match seal_wrapped_msek(&input.msek, &input.actor, input.credential_id, &cred, p) {
        Ok(b) => b,
        Err(_) => return,
    };
    let unwrapped = unseal_wrapped_msek(&blob, &cred).expect("round-trip must succeed");
    assert_eq!(*unwrapped, input.msek);
});
