//! Minimal C-ABI surface for the Python E2E test helper.
//!
//! The full C-ABI was deleted once the Windows app
//! moved to UniFFI bindings. This module restores only the subset that
//! `tests/e2e-unified/fauna_ffi.py` calls — the `fauna_post_build*`,
//! capability and folder builders plus the `FfiBuffer` /
//! `fauna_ffi_last_error` / `fauna_ffi_free_buffer` plumbing they depend on.
//! Production clients keep using UniFFI exports.
//!
//! **Test surface:** the whole module is declared in `lib.rs` behind the
//! `e2e-harness` feature, so no shipped `fauna-ffi` artifact exports any
//! `fauna_*` symbol from it (e2e convention 15). An export added here inherits
//! that gate; it never ships by default.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::slice;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(msg: String) {
    LAST_ERROR.with(|cell| {
        *cell.borrow_mut() = CString::new(msg).ok();
    });
}

/// Returns a NUL-terminated UTF-8 pointer to the most recent error on this
/// thread, or null if none. Pointer valid until the next FFI call on the
/// same thread.
#[unsafe(no_mangle)]
pub extern "C" fn fauna_ffi_last_error() -> *const c_char {
    LAST_ERROR.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or(std::ptr::null())
    })
}

/// Caller-visible buffer. Rust allocates; caller frees via
/// `fauna_ffi_free_buffer`.
#[repr(C)]
pub struct FfiBuffer {
    pub data: *mut u8,
    pub len: u32,
}

impl FfiBuffer {
    fn from_vec(v: Vec<u8>) -> Self {
        let len = v.len() as u32;
        let boxed = v.into_boxed_slice();
        let data = Box::into_raw(boxed) as *mut u8;
        FfiBuffer { data, len }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fauna_ffi_free_buffer(buf: FfiBuffer) {
    if buf.data.is_null() || buf.len == 0 {
        return;
    }
    // SAFETY: `buf.data`/`buf.len` were produced by `FfiBuffer::from_vec` (the
    // sole constructor), which boxes a `Vec<u8>` slice of exactly that length
    // via `Box::into_raw`; reconstructing the box with the same pointer/length
    // and dropping it is the documented, matching deallocation. The null/zero
    // check above rules out the zero-length case (`into_boxed_slice` on an
    // empty `Vec` may not yield a null/dereferenceable pointer).
    unsafe {
        let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            buf.data,
            buf.len as usize,
        ));
    }
}

macro_rules! cabi_try {
    ($expr:expr) => {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $expr)) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                set_last_error(e.to_string());
                return -1;
            }
            Err(_) => {
                set_last_error("panic in Rust FFI".into());
                return -1;
            }
        }
    };
}

/// # Safety
///
/// `ptr` must be null (with `len` 0) or point to `len` readable, initialized
/// bytes for the duration of this call — the same contract every caller in
/// this module documents in its own `# Safety` section.
unsafe fn slice_to_vec(ptr: *const u8, len: u32) -> Vec<u8> {
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    // SAFETY: `ptr` is non-null and `len` is non-zero here (the guard above
    // returned otherwise), so per this function's own `# Safety` contract the
    // caller guarantees `ptr` is valid for `len` readable bytes.
    unsafe { slice::from_raw_parts(ptr, len as usize) }.to_vec()
}

/// # Safety
///
/// `ptr` must be null or a valid NUL-terminated C string, readable for the
/// duration of this call.
unsafe fn cstr_to_string(ptr: *const c_char) -> Result<String, crate::FfiError> {
    if ptr.is_null() {
        return Err(crate::FfiError::General {
            msg: "null string pointer".into(),
        });
    }
    // SAFETY: `ptr` is non-null here (checked above); per this function's
    // `# Safety` contract the caller guarantees it is a valid NUL-terminated
    // C string.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    cstr.to_str()
        .map(|s| s.to_owned())
        .map_err(|e| crate::FfiError::General {
            msg: format!("invalid UTF-8: {e}"),
        })
}

/// Build a signed tier-**gated** feed `Post` plus its sealed full-body blob
/// (web paywall / gated-post fixtures — wraps the shared
/// `fauna_client_core::post::build_gated_post` seal helper).
///
/// `secret`/`secret_len` — 32-byte Ed25519 secret. `preview` / `full_body` /
/// `tier` — NUL-terminated UTF-8. `key_blob_ref` — 32 bytes (BLAKE3 of the
/// tier's `KeyBlob`). `period_key` — the tier's 32-byte period key.
/// `out_post` receives the `fauna.posts.create` wire bytes; `out_blob` the
/// encrypted full-body blob to upload (its BLAKE3 is the post's
/// `encrypted_ref`). Caller frees both via `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret`, `key_blob_ref`, and `period_key` must point to the stated byte
/// counts; `preview`, `full_body`, and `tier` must be valid NUL-terminated C
/// strings; `out_post` and `out_blob` must be valid, aligned, writable
/// `FfiBuffer` pointers. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_post_build_gated(
    secret: *const u8,
    secret_len: u32,
    preview: *const c_char,
    full_body: *const c_char,
    tier: *const c_char,
    tier_rank: u32,
    key_blob_ref: *const u8,
    period_key: *const u8,
    out_post: *mut FfiBuffer,
    out_blob: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `secret`/`secret_len`, `key_blob_ref`, `period_key`, and
    // `preview`/`full_body`/`tier` meet `slice_to_vec`/`cstr_to_string`'s
    // preconditions.
    let build = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let kp = crate::keypair_from_bytes(&secret_vec)?;
        let preview_s = cstr_to_string(preview)?;
        let full_s = cstr_to_string(full_body)?;
        let tier_s = cstr_to_string(tier)?;
        let kbr: [u8; 32] =
            slice_to_vec(key_blob_ref, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "key_blob_ref must be 32 bytes".into(),
                })?;
        let pk: [u8; 32] =
            slice_to_vec(period_key, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "period_key must be 32 bytes".into(),
                })?;
        fauna_client_core::post::build_gated_post(
            &kp, &preview_s, &full_s, &tier_s, tier_rank, kbr, &pk,
        )
        .map_err(|e| crate::FfiError::General { msg: e.0 })
    });
    // SAFETY: per the `# Safety` section above, `out_post` and `out_blob` are
    // valid, aligned, writable `FfiBuffer` pointers for the duration of this
    // call.
    unsafe {
        *out_post = FfiBuffer::from_vec(build.post_bytes);
        *out_blob = FfiBuffer::from_vec(build.encrypted_blob);
    }
    0
}

/// Build a community room's **birth record** for its founder — a fresh
/// binding birth salt (`fauna_mls::room_policy::binding_birth_salt` around 24
/// random bytes), the signed initial `RoomPolicy`
/// (`fauna_mls::room_policy::RoomPolicy::initial`) and a fresh group-reception
/// public key: everything `fauna.conversations.room.create` carries (the room
/// fixtures of the e2e harness, which founds a room over the wire with no app
/// engine).
///
/// The reception key's secret half is dropped: a harness founder seals under
/// the generation key its own mint returns
/// ([`fauna_room_build_generation_mint`]) and never opens a wrap.
///
/// `secret`/`secret_len` — the founder's 32-byte Ed25519 secret. `name` — a
/// NUL-terminated UTF-8 room name. `out_salt` receives the 32-byte salt;
/// `out_policy` the canonical-CBOR `SignedRoomPolicy`; `out_reception_pubkey`
/// the reception public key. Caller frees both buffers via
/// `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes; `name` must be a valid
/// NUL-terminated C string; `out_salt` must point to 32 writable bytes;
/// `out_policy` and `out_reception_pubkey` must be valid, aligned, writable
/// `FfiBuffer` pointers. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_room_build_birth(
    secret: *const u8,
    secret_len: u32,
    name: *const c_char,
    out_salt: *mut u8,
    out_policy: *mut FfiBuffer,
    out_reception_pubkey: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `secret`/`secret_len` and `name` meet `slice_to_vec`/`cstr_to_string`'s
    // preconditions.
    let (salt, policy, reception_pubkey) = cabi_try!(unsafe {
        let kp = crate::keypair_from_bytes(&slice_to_vec(secret, secret_len))?;
        let mut entropy = [0u8; 24];
        getrandom::fill(&mut entropy).map_err(|e| crate::FfiError::General {
            msg: format!("salt entropy: {e}"),
        })?;
        let salt = fauna_mls::room_policy::binding_birth_salt(&entropy);
        let name_s = cstr_to_string(name)?;
        let signed = fauna_mls::room_policy::RoomPolicy::initial(kp.actor_id(), Some(name_s))
            .sign(&kp)
            .map_err(|e| crate::FfiError::General { msg: e.to_string() })?;
        let policy = fauna_protocol::encode_canonical(&signed)
            .map_err(|e| crate::FfiError::General { msg: e.to_string() })?
            .to_vec();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        let reception = fauna_core::group_generation::GroupReceptionKeyRecord::mint(now_ms);
        let pubkey = reception
            .reception_pubkey()
            .map_err(|e| crate::FfiError::General { msg: e.to_string() })?;
        Ok::<_, crate::FfiError>((salt, policy, pubkey))
    });
    // SAFETY: per the `# Safety` section above, `out_salt` points to 32
    // writable bytes and both buffers are valid, aligned, writable `FfiBuffer`
    // pointers for the duration of this call.
    unsafe {
        std::ptr::copy_nonoverlapping(salt.as_ptr(), out_salt, 32);
        *out_policy = FfiBuffer::from_vec(policy);
        *out_reception_pubkey = FfiBuffer::from_vec(reception_pubkey);
    }
    0
}

/// Build one community-room **generation mint** over the wrap targets the
/// caller read back from `fauna.conversations.room.list_roster` — the owner's
/// rotation (`conversation-rooms.md` § The three classes → *Community*), over
/// the same `build_group_mint` the room engine's
/// `MlsEngine::build_room_generation_mint` calls.
///
/// The targets come as `count` parallel rows: `entry_ids` and `actors` are
/// `count * 32` bytes each, `reception_lens` is `count` lengths, and
/// `reception_pubkeys` is the keys concatenated in that order. Leaving a
/// member out of the targets is how an owner rotates it — or the home nest —
/// out. `parents` is `parents_count * 32` bytes: empty for a room's first
/// mint, else the tip it replaces.
///
/// `out_mint` receives the canonical-CBOR mint record for
/// `fauna.conversations.room.publish_generation`; `out_gen_key` the 32-byte
/// generation key; `out_generation_id` its 32-byte id. Caller frees `out_mint`
/// via `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes; `entry_ids` and
/// `actors` to `count * 32`; `reception_lens` to `count` `u32`s;
/// `reception_pubkeys` to the sum of those lengths; `parents` to
/// `parents_count * 32` (null when zero); `out_mint` must be a valid, aligned,
/// writable `FfiBuffer` pointer and `out_gen_key` / `out_generation_id` must
/// point to 32 writable bytes each. All pointers must remain valid for the
/// call.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn fauna_room_build_generation_mint(
    secret: *const u8,
    secret_len: u32,
    count: u32,
    entry_ids: *const u8,
    actors: *const u8,
    reception_lens: *const u32,
    reception_pubkeys: *const u8,
    parents: *const u8,
    parents_count: u32,
    minted_at_ms: i64,
    out_mint: *mut FfiBuffer,
    out_gen_key: *mut u8,
    out_generation_id: *mut u8,
) -> i32 {
    fn bad(msg: &str) -> crate::FfiError {
        crate::FfiError::General { msg: msg.into() }
    }
    // SAFETY: per the `# Safety` section above, every input pointer is valid
    // for the byte counts its length arguments state.
    let built = cabi_try!(unsafe {
        let kp = crate::keypair_from_bytes(&slice_to_vec(secret, secret_len))?;
        let n = count as usize;
        let entry_ids = slice_to_vec(entry_ids, count * 32);
        let actors = slice_to_vec(actors, count * 32);
        let lens: &[u32] = if n == 0 || reception_lens.is_null() {
            &[]
        } else {
            slice::from_raw_parts(reception_lens, n)
        };
        if entry_ids.len() != n * 32 || actors.len() != n * 32 || lens.len() != n {
            return Err(bad(
                "roster rows must be count * 32 bytes with count lengths",
            ));
        }
        let total: u32 = lens.iter().sum();
        let keys = slice_to_vec(reception_pubkeys, total);
        let mut targets = Vec::with_capacity(n);
        let mut at = 0usize;
        for i in 0..n {
            let len = lens[i] as usize;
            let mut entry = [0u8; 32];
            entry.copy_from_slice(&entry_ids[i * 32..(i + 1) * 32]);
            let mut actor = [0u8; 32];
            actor.copy_from_slice(&actors[i * 32..(i + 1) * 32]);
            targets.push(fauna_core::group_scope::RosterMember {
                entry_id: entry,
                member_actor: fauna_core::identity::ActorId(actor),
                reception_pubkey: keys[at..at + len].to_vec(),
                enrolled_at_ms: 0,
            });
            at += len;
        }
        let parent_bytes = slice_to_vec(parents, parents_count * 32);
        let parents: Vec<[u8; 32]> = parent_bytes.as_chunks::<32>().0.to_vec();
        let built = fauna_mls::wrapped_blob::group_generation_wraps::build_group_mint(
            &targets,
            parents,
            Vec::new(),
            kp.signing_key(),
            Vec::new(),
            minted_at_ms,
        )
        .map_err(|e| bad(&e.to_string()))?;
        let record = fauna_protocol::encode_canonical(&built.record)
            .map_err(|e| bad(&e.to_string()))?
            .to_vec();
        Ok::<_, crate::FfiError>((record, *built.gen_key.as_bytes(), built.generation_id))
    });
    // SAFETY: per the `# Safety` section above, `out_mint` is a valid writable
    // `FfiBuffer` pointer and the two key outputs point to 32 writable bytes.
    unsafe {
        *out_mint = FfiBuffer::from_vec(built.0);
        std::ptr::copy_nonoverlapping(built.1.as_ptr(), out_gen_key, 32);
        std::ptr::copy_nonoverlapping(built.2.as_ptr(), out_generation_id, 32);
    }
    0
}

/// Build a signed **room-restricted** feed `Post` addressed to a community
/// room, plus its sealed full-body blob — the room twin of
/// [`fauna_post_build_gated`], wrapping the shared
/// `fauna_client_core::post::build_room_post_at` under
/// `room_post_base_key(gen_key)` with a fresh `seal_id`.
///
/// `room` — the room's 32-byte channel id; `generation` / `gen_key` — the
/// generation id and key the room's mint returned
/// ([`fauna_room_build_generation_mint`]). `out_post` receives the
/// `fauna.posts.create` wire bytes, `out_blob` the sealed body to upload
/// first. Caller frees both via `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes and `room`,
/// `generation`, `gen_key` to 32 each; `preview` and `full_body` must be valid
/// NUL-terminated C strings; `out_post` and `out_blob` must be valid, aligned,
/// writable `FfiBuffer` pointers. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn fauna_post_build_room(
    secret: *const u8,
    secret_len: u32,
    preview: *const c_char,
    full_body: *const c_char,
    room: *const u8,
    generation: *const u8,
    gen_key: *const u8,
    out_post: *mut FfiBuffer,
    out_blob: *mut FfiBuffer,
) -> i32 {
    fn arr32(v: Vec<u8>, what: &str) -> Result<[u8; 32], crate::FfiError> {
        v.try_into().map_err(|_| crate::FfiError::General {
            msg: format!("{what} must be 32 bytes"),
        })
    }
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // input meets `slice_to_vec`/`cstr_to_string`'s preconditions.
    let build = cabi_try!(unsafe {
        let kp = crate::keypair_from_bytes(&slice_to_vec(secret, secret_len))?;
        let preview_s = cstr_to_string(preview)?;
        let full_s = cstr_to_string(full_body)?;
        let room = arr32(slice_to_vec(room, 32), "room")?;
        let generation = arr32(slice_to_vec(generation, 32), "generation")?;
        let key = fauna_core::crypto::GenerationKey::from_bytes(arr32(
            slice_to_vec(gen_key, 32),
            "gen_key",
        )?);
        let seal_id = fauna_client_core::post::mint_seal_id()
            .map_err(|e| crate::FfiError::General { msg: e.0 })?;
        fauna_client_core::post::build_room_post_at(
            &kp,
            &preview_s,
            fauna_core::data::PostBody::Text {
                content: full_s,
                facets: vec![],
            },
            room,
            fauna_core::room_post::RoomPostSeal::Community { generation },
            &fauna_core::group_content::room_post_base_key(&key),
            seal_id,
            &fauna_client_core::post::PostAuthoring::now(),
        )
        .map_err(|e| crate::FfiError::General { msg: e.0 })
    });
    // SAFETY: per the `# Safety` section above, `out_post` and `out_blob` are
    // valid, aligned, writable `FfiBuffer` pointers for the duration of this
    // call.
    unsafe {
        *out_post = FfiBuffer::from_vec(build.post_bytes);
        *out_blob = FfiBuffer::from_vec(build.encrypted_blob);
    }
    0
}

/// Sign a community room's **labeler set** — the canonical-CBOR
/// `SignedRoomLabelers` that `fauna.conversations.room.set_labelers` carries
/// (`fauna_mls::room_policy::RoomLabelers`, sorted and de-duplicated there).
///
/// `labelers` is `count * 32` bytes of labeler ids. `out` receives the signed
/// set; the caller frees it via `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes, `room` to 32, and
/// `labelers` to `count * 32` (null when zero); `out` must be a valid,
/// aligned, writable `FfiBuffer` pointer. All pointers must remain valid for
/// the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_room_sign_labelers(
    secret: *const u8,
    secret_len: u32,
    room: *const u8,
    version: u64,
    labelers: *const u8,
    count: u32,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, every input pointer is valid
    // for the byte counts its length arguments state.
    let signed = cabi_try!(unsafe {
        let kp = crate::keypair_from_bytes(&slice_to_vec(secret, secret_len))?;
        let room: [u8; 32] =
            slice_to_vec(room, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "room must be 32 bytes".into(),
                })?;
        let ids = slice_to_vec(labelers, count * 32);
        let set = fauna_mls::room_policy::RoomLabelers::new(
            room,
            version,
            ids.as_chunks::<32>()
                .0
                .iter()
                .map(|c| fauna_core::identity::ActorId(*c)),
        )
        .sign(&kp)
        .map_err(|e| crate::FfiError::General { msg: e.to_string() })?;
        fauna_protocol::encode_canonical(&set)
            .map(|b| b.to_vec())
            .map_err(|e| crate::FfiError::General { msg: e.to_string() })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable `FfiBuffer` pointer for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(signed);
    }
    0
}

/// Sign an invitation of `invitee` into a community room as a **member** — the
/// canonical-CBOR `SignedRoomInvite` that `fauna.conversations.room.invite`
/// carries (`fauna_mls::room_policy::RoomInvite`, signed by the inviter's
/// key), read under the room's policy `policy_version`.
///
/// `out` receives the signed invitation; the caller frees it via
/// `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes, `room` and `invitee` to
/// 32 each; `out` must be a valid, aligned, writable `FfiBuffer` pointer. All
/// pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_room_sign_member_invite(
    secret: *const u8,
    secret_len: u32,
    room: *const u8,
    invitee: *const u8,
    policy_version: u64,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, every input pointer is valid
    // for the byte counts its length arguments state.
    let signed = cabi_try!(unsafe {
        let kp = crate::keypair_from_bytes(&slice_to_vec(secret, secret_len))?;
        let invitee: [u8; 32] =
            slice_to_vec(invitee, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "invitee must be 32 bytes".into(),
                })?;
        let invite = fauna_mls::room_policy::RoomInvite {
            room_id: slice_to_vec(room, 32),
            invitee: fauna_core::identity::ActorId(invitee),
            role: fauna_mls::room_policy::RoomRole::Member,
            policy_version,
        }
        .sign(&kp)
        .map_err(|e| crate::FfiError::General { msg: e.to_string() })?;
        fauna_protocol::encode_canonical(&invite)
            .map(|b| b.to_vec())
            .map_err(|e| crate::FfiError::General { msg: e.to_string() })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable `FfiBuffer` pointer for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(signed);
    }
    0
}

/// A fresh group-reception public key — the wrap target an invitee names in
/// `fauna.conversations.room.accept_invite`
/// (`fauna_core::group_generation::GroupReceptionKeyRecord`). The secret half
/// is dropped, as [`fauna_room_build_birth`] drops the founder's: a harness
/// member never opens a wrap.
///
/// `out` receives the public key; the caller frees it via
/// `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `out` must be a valid, aligned, writable `FfiBuffer` pointer for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_room_mint_reception_pubkey(out: *mut FfiBuffer) -> i32 {
    let pubkey = cabi_try!({
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        fauna_core::group_generation::GroupReceptionKeyRecord::mint(now_ms)
            .reception_pubkey()
            .map_err(|e| crate::FfiError::General { msg: e.to_string() })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable `FfiBuffer` pointer for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(pubkey);
    }
    0
}

/// Build a canonical-CBOR capability `GrantBlob` carrying one
/// `content.read{post:tier}` scope wrapping the tier's period key — the
/// owner-side mint input for `fauna.capabilities.mint` (web paywall; wraps
/// `fauna_mls::wrapped_blob::build_grant_blob` like the UniFFI
/// `build_capability_grant_blob`, shaped for the Python e2e harness).
///
/// `holder_mlkem_ek` — null+0 for a classical wrap, or the holder's published
/// 1184-byte ML-KEM ek for the X-Wing hybrid wrap.
///
/// # Safety
///
/// `owner` (32), `grant_id` (16), `holder_pubkey` (32), and `period_key` (32)
/// must point to the stated byte counts; `holder_mlkem_ek` must be null (with
/// len 0) or point to `holder_mlkem_ek_len` readable bytes; `tier` must be a
/// valid NUL-terminated C string; `out` must be a valid, aligned, writable
/// `FfiBuffer` pointer. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_capability_build_post_grant(
    owner: *const u8,
    grant_id: *const u8,
    holder_pubkey: *const u8,
    holder_mlkem_ek: *const u8,
    holder_mlkem_ek_len: u32,
    epoch_start: u64,
    epoch_end: u64,
    tier: *const c_char,
    period_key: *const u8,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple, build_grant_blob};
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `owner`, `grant_id`, `holder_pubkey`, `period_key`,
    // `holder_mlkem_ek`/`holder_mlkem_ek_len`, and `tier` meet
    // `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let owner_a: [u8; 32] =
            slice_to_vec(owner, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "owner must be 32 bytes".into(),
                })?;
        let grant_a: [u8; 16] =
            slice_to_vec(grant_id, 16)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "grant_id must be 16 bytes".into(),
                })?;
        let holder_a: [u8; 32] =
            slice_to_vec(holder_pubkey, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "holder_pubkey must be 32 bytes".into(),
                })?;
        let ek = if holder_mlkem_ek.is_null() || holder_mlkem_ek_len == 0 {
            None
        } else {
            Some(slice_to_vec(holder_mlkem_ek, holder_mlkem_ek_len))
        };
        let tier_s = cstr_to_string(tier)?;
        let pk = slice_to_vec(period_key, 32);
        if pk.len() != 32 {
            return Err(crate::FfiError::General {
                msg: "period_key must be 32 bytes".into(),
            });
        }
        let blob = build_grant_blob(
            &owner_a,
            &grant_a,
            &holder_a,
            ek.as_deref(),
            GrantWindow(epoch_start, epoch_end),
            &[(
                ScopeTuple {
                    class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
                    kind: Some(ScopeTuple::KIND_POST.to_string()),
                    tier: Some(tier_s),
                    set: None,
                    factor: None,
                },
                Some(pk),
            )],
        )
        .map_err(|e| crate::FfiError::General {
            msg: format!("build grant blob: {e}"),
        })?;
        blob.to_canonical_bytes()
            .map_err(|e| crate::FfiError::General {
                msg: format!("encode grant blob: {e}"),
            })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Chunk — and, for a paywalled set, content-key-seal — a file, returning the
/// bundle the Python folder-paywall e2e harness uploads through the chunk
/// routes. The **folder** analogue of `fauna_post_build_gated`: it produces
/// exactly the at-rest shape `web_content::file_bytes::seed_synced_file` (the
/// nest read path's reference test helper) writes, so a synced `web`-mode file
/// serves correctly and — sealed — opens under its content-key grant. The two
/// helpers must not drift.
///
/// `content_key` — null for a **plaintext** set (store key == plaintext chunk
/// hash, `stored_hashes == None`, today's public web hosting), or a 32-byte
/// pointer for a **content-key-sealed** set (every chunk AEAD-sealed under
/// `chunk_crypto`, addressed by its ciphertext hash via `manifest.stored_hashes`
/// — the M2 shape, `mls-group-key-material.md` § M2).
///
/// `out` receives canonical CBOR `[manifest_bytes, [[store_key, body], …]]`
/// (positional, so no serde-derive dep): `manifest_bytes` is the canonical
/// `ChunkManifest` to `POST /api/v1/manifests`; each `body` is the bytes to
/// `POST /api/v1/chunks` with `X-Content-Hash: store_key` — a sealed body is
/// keyed by its own `blake3` (the ciphertext hash), a plaintext body is the
/// FRAMED chunk keyed by the plaintext hash it unframes to — and the harness
/// asserts the upload reply echoes `store_key`.
///
/// # Safety
///
/// `content` must point to `content_len` readable bytes (or be null with
/// `content_len` 0); `content_key` must be null or point to 32 readable bytes;
/// `out` must be a valid, aligned, writable `FfiBuffer` pointer. All pointers
/// must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_folder_seal_file(
    content: *const u8,
    content_len: u32,
    content_key: *const u8,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_protocol::ByteBuf;
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `content`/`content_len` and `content_key` meet `slice_to_vec`'s
    // preconditions.
    let bundle = cabi_try!(unsafe {
        let content_vec = slice_to_vec(content, content_len);
        let key: Option<[u8; 32]> = if content_key.is_null() {
            None
        } else {
            Some(slice_to_vec(content_key, 32).try_into().map_err(|_| {
                crate::FfiError::General {
                    msg: "content_key must be 32 bytes".into(),
                }
            })?)
        };
        let mut manifest = fauna_core::chunker::chunk_file(&content_vec);
        let chunks = fauna_core::chunker::extract_chunks(&content_vec, &manifest);
        let mut out_chunks: Vec<(ByteBuf, ByteBuf)> = Vec::with_capacity(chunks.len());
        match key {
            None => {
                // Plaintext: the store key is the plaintext chunk hash and the
                // body is the FRAMED plaintext chunk (what public web hosting
                // serves) — framed through the one door, `chunk_seal`, exactly
                // as `seed_synced_file`'s plaintext arm and every production
                // writer do.
                for (hash, data) in &chunks {
                    let body = fauna_core::chunk_seal::FramedChunk::frame(hash, data)
                        .map_err(|e| crate::FfiError::General {
                            msg: format!("{e:#}"),
                        })?
                        .into_body();
                    out_chunks.push((ByteBuf::from(hash.digest().to_vec()), ByteBuf::from(body)));
                }
            }
            Some(k) => {
                // Sealed: each chunk goes through the ONE seal door
                // (`fauna_core::chunk_seal` — frame, then AEAD keyed by the
                // plaintext hash), stored under its ciphertext hash via
                // `stored_hashes`. The same function the sync engine and the
                // Go WebDAV MDA seal with, so this harness fixture is
                // byte-identical to a production upload and can never be the
                // second framing that rests two plaintexts under one nonce.
                let mut stored = Vec::with_capacity(chunks.len());
                for (hash, data) in &chunks {
                    let (store_key, ciphertext) =
                        fauna_core::chunk_seal::seal_chunk_body(hash, data, &k).map_err(|e| {
                            crate::FfiError::General {
                                msg: format!("seal chunk: {e}"),
                            }
                        })?;
                    out_chunks.push((
                        ByteBuf::from(store_key.digest().to_vec()),
                        ByteBuf::from(ciphertext),
                    ));
                    stored.push(store_key);
                }
                manifest.stored_hashes = Some(stored);
            }
        }
        // The wire form, as every production writer encodes it: a sealed
        // manifest's plaintext hashes ride only sealed under `k`.
        let manifest_bytes = manifest
            .wire_form(key.as_ref())
            .and_then(|m| fauna_core::encoding::canonical_encode(&m).map_err(Into::into))
            .map_err(|e| crate::FfiError::General {
                msg: format!("encode manifest: {e:#}"),
            })?;
        fauna_core::encoding::canonical_encode(&(ByteBuf::from(manifest_bytes), out_chunks))
            .map_err(|e| crate::FfiError::General {
                msg: format!("encode seal bundle: {e}"),
            })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bundle);
    }
    0
}

/// [`fauna_folder_seal_file`] for a file in an **owner-only** set: every chunk
/// sealed under the owner root `secret` derives
/// (`BackupKey::derive(seed).convergent_chunk_root()` — the root an unbound
/// set's writer seals under, and the one `fauna_client_share::mint_private_link`
/// opens the manifest's hashes with), so a private share link can be made to
/// it. Same `out` bundle as [`fauna_folder_seal_file`]; the root never leaves
/// Rust.
///
/// # Safety
///
/// `secret` must point to 32 readable bytes; `content` / `out` as
/// [`fauna_folder_seal_file`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_folder_seal_owner_file(
    secret: *const u8,
    content: *const u8,
    content_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above.
    let secret = match unsafe { read_32(secret, "secret") } {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e.to_string());
            return -1;
        }
    };
    let root = fauna_core::crypto::BackupKey::derive(&secret).convergent_chunk_root();
    // SAFETY: `root` is 32 readable bytes for the call; the rest is forwarded
    // from this export's own contract.
    unsafe { fauna_folder_seal_file(content, content_len, root.as_ptr(), out) }
}

/// Strip one stored chunk body's frame — the shared strict unframe
/// (`fauna_core::compress::unframe_strict_bounded`, bomb-bounded), the framed
/// half of the walk every file-sync reader runs — so a Python harness reading a
/// plaintext folder by
/// hand gets the file's bytes rather than its frames, without reimplementing
/// the codec (zstd included). A body with no frame prefix is an error.
///
/// `out` receives the unframed bytes.
///
/// # Safety
///
/// `body` must point to `body_len` readable bytes (or be null with `body_len`
/// 0); `out` must be a valid, aligned, writable `FfiBuffer` pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_chunk_unframe(
    body: *const u8,
    body_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `body`/`body_len` meet `slice_to_vec`'s preconditions.
    let unframed = cabi_try!(unsafe {
        let body_vec = slice_to_vec(body, body_len);
        fauna_core::compress::unframe_strict_bounded(
            &body_vec,
            fauna_core::compress::MAX_DECOMPRESSED_CHUNK,
        )
        .map_err(|e| crate::FfiError::General {
            msg: format!("unframe chunk: {e:#}"),
        })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(unframed);
    }
    0
}

/// Build a canonical-CBOR capability `GrantBlob` carrying one
/// `content.read{folder:set}` scope, wrapping the set's content key under a
/// single generation (`epoch = version`) — the folder analogue of
/// `fauna_capability_build_post_grant` and the Python e2e twin of the
/// production `mint_folder_grant`. One generation suffices for a fixture;
/// multi-generation grants (a CRDT-merge/rotation edge) are
/// `mint_folder_grant`'s job.
///
/// The wrap is AAD-bound to `(owner, content.read, folder, set, version)`, so
/// the holder can present it only for exactly this set + generation — the
/// substitution refusal S2/S4 unit-pin, now exercised end-to-end.
///
/// `holder_mlkem_ek` — null+0 for a classical wrap, or the holder's published
/// ML-KEM ek for the X-Wing hybrid wrap.
///
/// # Safety
///
/// `owner` (32), `grant_id` (16), `holder_pubkey` (32), and `content_key` (32)
/// must point to the stated byte counts; `holder_mlkem_ek` must be null (with
/// len 0) or point to `holder_mlkem_ek_len` readable bytes; `set_name` must be a
/// valid NUL-terminated C string; `out` must be a valid, aligned, writable
/// `FfiBuffer` pointer. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_capability_build_folder_grant(
    owner: *const u8,
    grant_id: *const u8,
    holder_pubkey: *const u8,
    holder_mlkem_ek: *const u8,
    holder_mlkem_ek_len: u32,
    epoch_start: u64,
    epoch_end: u64,
    set_name: *const c_char,
    version: u64,
    content_key: *const u8,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple, build_grant_blob_with_epochs};
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `owner`, `grant_id`, `holder_pubkey`, `content_key`,
    // `holder_mlkem_ek`/`holder_mlkem_ek_len`, and `set_name` meet
    // `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let owner_a: [u8; 32] =
            slice_to_vec(owner, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "owner must be 32 bytes".into(),
                })?;
        let grant_a: [u8; 16] =
            slice_to_vec(grant_id, 16)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "grant_id must be 16 bytes".into(),
                })?;
        let holder_a: [u8; 32] =
            slice_to_vec(holder_pubkey, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "holder_pubkey must be 32 bytes".into(),
                })?;
        let ek = if holder_mlkem_ek.is_null() || holder_mlkem_ek_len == 0 {
            None
        } else {
            Some(slice_to_vec(holder_mlkem_ek, holder_mlkem_ek_len))
        };
        let set_s = cstr_to_string(set_name)?;
        let ck = slice_to_vec(content_key, 32);
        if ck.len() != 32 {
            return Err(crate::FfiError::General {
                msg: "content_key must be 32 bytes".into(),
            });
        }
        let tuple = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
            kind: Some(ScopeTuple::KIND_FOLDER.to_string()),
            tier: None,
            set: Some(set_s),
            factor: None,
        };
        let blob = build_grant_blob_with_epochs(
            &owner_a,
            &grant_a,
            &holder_a,
            ek.as_deref(),
            GrantWindow(epoch_start, epoch_end),
            &[(tuple, vec![(Some(version), ck)])],
        )
        .map_err(|e| crate::FfiError::General {
            msg: format!("build folder grant blob: {e}"),
        })?;
        blob.to_canonical_bytes()
            .map_err(|e| crate::FfiError::General {
                msg: format!("encode grant blob: {e}"),
            })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Build the canonical-CBOR `WrappedScopeKey` for ONE content-key generation of a
/// `content.read{folder:set}` grant — the wire form each `appended_keys` entry a
/// `fauna.capabilities.renew` carries (`libs/fauna-mls/.../format.rs`
/// `WrappedScopeKey::to_canonical_bytes`), and the Python e2e twin of the per-
/// generation wraps the production `rotate_paywall_grant` re-provisions. The
/// paywall-folder rotation test uses it to append a NEW generation's key to a
/// standing grant so the newest-sealed bytes serve.
///
/// The wrap's AAD binds `(owner, content.read, folder, set, version)` — **not**
/// the grant id or window (`AadBinding::for_capability`) — so a lone wrap built
/// here (with a throwaway index/window internally) is valid appended to the live
/// grant under any id; the renew request carries the grant id + window separately.
///
/// `holder_mlkem_ek` — null+0 for a classical wrap, or the holder's published
/// ML-KEM ek for the X-Wing hybrid wrap.
///
/// # Safety
///
/// `owner` (32), `holder_pubkey` (32), and `content_key` (32) must point to the
/// stated byte counts; `holder_mlkem_ek` must be null (with len 0) or point to
/// `holder_mlkem_ek_len` readable bytes; `set_name` must be a valid NUL-terminated
/// C string; `out` must be a valid, aligned, writable `FfiBuffer` pointer. All
/// pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_capability_build_folder_scope_wrap(
    owner: *const u8,
    holder_pubkey: *const u8,
    holder_mlkem_ek: *const u8,
    holder_mlkem_ek_len: u32,
    set_name: *const c_char,
    version: u64,
    content_key: *const u8,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_mls::wrapped_blob::{GrantWindow, ScopeTuple, build_grant_blob_with_epochs};
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `owner`, `holder_pubkey`, `content_key`,
    // `holder_mlkem_ek`/`holder_mlkem_ek_len`, and `set_name` meet
    // `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let owner_a: [u8; 32] =
            slice_to_vec(owner, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "owner must be 32 bytes".into(),
                })?;
        let holder_a: [u8; 32] =
            slice_to_vec(holder_pubkey, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "holder_pubkey must be 32 bytes".into(),
                })?;
        let ek = if holder_mlkem_ek.is_null() || holder_mlkem_ek_len == 0 {
            None
        } else {
            Some(slice_to_vec(holder_mlkem_ek, holder_mlkem_ek_len))
        };
        let set_s = cstr_to_string(set_name)?;
        let ck = slice_to_vec(content_key, 32);
        if ck.len() != 32 {
            return Err(crate::FfiError::General {
                msg: "content_key must be 32 bytes".into(),
            });
        }
        let tuple = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
            kind: Some(ScopeTuple::KIND_FOLDER.to_string()),
            tier: None,
            set: Some(set_s),
            factor: None,
        };
        // Throwaway grant id + window: the wrap's AAD binds neither, so extracting
        // the single wrapped_key yields a renew-ready `appended_keys` entry.
        let blob = build_grant_blob_with_epochs(
            &owner_a,
            &[0u8; 16],
            &holder_a,
            ek.as_deref(),
            GrantWindow(0, u64::MAX),
            &[(tuple, vec![(Some(version), ck)])],
        )
        .map_err(|e| crate::FfiError::General {
            msg: format!("build folder scope wrap: {e}"),
        })?;
        let wrap =
            blob.wrapped_keys
                .into_iter()
                .next()
                .ok_or_else(|| crate::FfiError::General {
                    msg: "built grant had no wrapped key".into(),
                })?;
        wrap.to_canonical_bytes()
            .map_err(|e| crate::FfiError::General {
                msg: format!("encode scope wrap: {e}"),
            })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Read a 32-byte array behind `ptr`, naming `what` in the refusal.
///
/// # Safety
///
/// `ptr` must be null or point to 32 readable bytes.
unsafe fn read_32(ptr: *const u8, what: &str) -> Result<[u8; 32], crate::FfiError> {
    // SAFETY: per this function's `# Safety` section.
    unsafe { slice_to_vec(ptr, 32) }
        .try_into()
        .map_err(|_| crate::FfiError::General {
            msg: format!("{what} must be 32 bytes"),
        })
}

/// Build the canonical-CBOR **consent-time grant to a third-party principal**
/// over `ext.*` kinds — the Python e2e twin of the owner's mint inside
/// `fauna_client_capabilities::ext_consent::prepare_ext_consent_grant`, minus
/// the two account-store writes (the kind-manifest plane row and the ledger
/// `Mint`) a harness holding no account store cannot make; both are pinned in
/// Rust. It runs the production mint itself,
/// `fauna_client_capabilities::mint_ext_kinds_grant`, over the delegable
/// branch derived from the owner's identity seed exactly as
/// `ConsentGrantSeams::from_keypair` derives it, so the pair a principal
/// opens is the one the owner's replicas seal under.
///
/// `kinds` is newline-separated full `ext.*` kind strings (a wildcard already
/// expanded against the manifest, as the entry point does). `writer` — null
/// for a read-only grant, else the principal's attested Ed25519 writer key,
/// which each kind's keyless `content.write` tuple names.
///
/// # Safety
///
/// `identity_seed` (32), `grant_id` (16) and `holder_x25519` (32) must point
/// to the stated byte counts; `writer` must be null or point to 32 bytes;
/// `kinds` must be a valid NUL-terminated C string; `out` must be a valid,
/// aligned, writable `FfiBuffer` pointer. All pointers must remain valid for
/// the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_capability_build_ext_kinds_grant(
    identity_seed: *const u8,
    grant_id: *const u8,
    holder_x25519: *const u8,
    epoch_start: u64,
    epoch_end: u64,
    kinds: *const c_char,
    writer: *const u8,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_core::crypto::{BackupKey, DelegableSchedule};
    use fauna_mls::wrapped_blob::GrantWindow;
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let seed = read_32(identity_seed, "identity_seed")?;
        let grant_a: [u8; 16] =
            slice_to_vec(grant_id, 16)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "grant_id must be 16 bytes".into(),
                })?;
        let holder = read_32(holder_x25519, "holder_x25519")?;
        let writer = if writer.is_null() {
            None
        } else {
            Some(read_32(writer, "writer")?)
        };
        let kinds = cstr_to_string(kinds)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                l.parse::<fauna_protocol::ext_kind::ExtKind>().map_err(|e| {
                    crate::FfiError::General {
                        msg: format!("kind {l:?}: {e}"),
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let keypair = fauna_core::identity::ActorKeypair::from_secret(seed);
        let delegable = DelegableSchedule::derive(&BackupKey::derive(keypair.secret_bytes()));
        let blob = fauna_client_capabilities::mint_ext_kinds_grant(
            &delegable,
            &keypair.actor_id().0,
            &grant_a,
            &holder,
            None,
            GrantWindow(epoch_start, epoch_end),
            &kinds,
            writer.as_ref(),
        )
        .map_err(|e| crate::FfiError::General {
            msg: format!("mint ext kinds grant: {e}"),
        })?;
        blob.to_canonical_bytes()
            .map_err(|e| crate::FfiError::General {
                msg: format!("encode grant blob: {e}"),
            })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Build the canonical-CBOR **`fauna:personalization:rw` grant twin** to a
/// third-party principal (`authorization-server.md` § Scope grammar → *The
/// fifth arm*): two `content.read` tuples, the delegable pairs of
/// `fauna.state.personalization` and `fauna.personalization.model`, wrapped to
/// `holder_x25519`. The Python e2e twin of the owner's mint inside
/// `fauna_client_capabilities::ext_consent::prepare_ext_consent_grant`, minus
/// the ledger `Mint` (pinned in Rust): it runs the production
/// `fauna_client_capabilities::mint_personalization_grant` over the delegable
/// branch derived from the owner's identity seed exactly as
/// `ConsentGrantSeams::from_keypair` derives it.
///
/// # Safety
///
/// `identity_seed` (32), `grant_id` (16) and `holder_x25519` (32) must point
/// to the stated byte counts; `out` must be a valid, aligned, writable
/// `FfiBuffer` pointer. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_capability_build_personalization_grant(
    identity_seed: *const u8,
    grant_id: *const u8,
    holder_x25519: *const u8,
    epoch_start: u64,
    epoch_end: u64,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_core::crypto::{BackupKey, DelegableSchedule};
    use fauna_mls::wrapped_blob::GrantWindow;
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let seed = read_32(identity_seed, "identity_seed")?;
        let grant_a: [u8; 16] =
            slice_to_vec(grant_id, 16)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "grant_id must be 16 bytes".into(),
                })?;
        let holder = read_32(holder_x25519, "holder_x25519")?;
        let keypair = fauna_core::identity::ActorKeypair::from_secret(seed);
        let delegable = DelegableSchedule::derive(&BackupKey::derive(keypair.secret_bytes()));
        let blob = fauna_client_capabilities::mint_personalization_grant(
            &delegable,
            &keypair.actor_id().0,
            &grant_a,
            &holder,
            GrantWindow(epoch_start, epoch_end),
        )
        .map_err(|e| crate::FfiError::General {
            msg: format!("mint personalization grant: {e}"),
        })?;
        blob.to_canonical_bytes()
            .map_err(|e| crate::FfiError::General {
                msg: format!("encode grant blob: {e}"),
            })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Seal one row of a **delegable** account-plane kind under the scope `state`
/// **as the owner**: the kind's pair from the delegable branch of the owner's
/// identity seed (`DelegableSchedule::for_kind`, the pair a grant twin wraps),
/// signed by `writer_secret` — what one of the owner's replicas puts on the
/// delegable feed. The e2e harness's owner leg for a principal that opens
/// `state` rows with a granted pair (`fauna:personalization:rw`); it carries
/// no merge metadata, the open being the observable.
///
/// `out` receives canonical CBOR `{"item_key": bytes(32), "envelope": bytes}`.
///
/// # Safety
///
/// `identity_seed` and `writer_secret` must point to 32 bytes each; `kind` and
/// `key` must be valid NUL-terminated C strings; `value` must point to
/// `value_len` readable bytes (or be null with len 0); `out` must be a valid,
/// aligned, writable `FfiBuffer` pointer. All pointers must remain valid for
/// the call.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)] // a flat C ABI
pub unsafe extern "C" fn fauna_account_state_seal_delegable_row(
    identity_seed: *const u8,
    writer_secret: *const u8,
    kind: *const c_char,
    key: *const c_char,
    value: *const u8,
    value_len: u32,
    writer_seq: u64,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_core::account_entry_crypto::{EntryCoordinates, EntryPlaintext, seal_entry};
    use fauna_core::crypto::{BackupKey, DelegableSchedule};
    use fauna_protocol::ByteBuf;
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let general = |msg: String| crate::FfiError::General { msg };
        let seed = read_32(identity_seed, "identity_seed")?;
        let writer =
            ed25519_dalek::SigningKey::from_bytes(&read_32(writer_secret, "writer_secret")?);
        let kind_s = cstr_to_string(kind)?;
        let keypair = fauna_core::identity::ActorKeypair::from_secret(seed);
        let keys = DelegableSchedule::derive(&BackupKey::derive(keypair.secret_bytes()))
            .for_kind(&kind_s)
            .into_keys();
        let sealed = seal_entry(
            &keys,
            &EntryCoordinates {
                writer_id: writer.verifying_key().to_bytes(),
                writer_seq,
                scope: fauna_protocol::account_state::ACCOUNT_STATE_SCOPE,
            },
            &EntryPlaintext {
                kind: kind_s,
                key: cstr_to_string(key)?,
                merge_meta: None,
                value: ByteBuf::from(slice_to_vec(value, value_len)),
                tombstone: false,
            },
            &writer,
        )
        .map_err(|e| general(format!("seal: {e:#}")))?;
        let reply: std::collections::BTreeMap<&str, ByteBuf> = [
            ("item_key", ByteBuf::from(sealed.item_key.to_vec())),
            ("envelope", ByteBuf::from(sealed.envelope)),
        ]
        .into_iter()
        .collect();
        fauna_core::encoding::canonical_encode(&reply)
            .map_err(|e| general(format!("encode sealed row: {e}")))
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Open one `state` row **as a `fauna:personalization:rw` principal**: the
/// twin's two pairs out of `grant_blob` with the holder's X25519 secret (the
/// production `fauna_client_capabilities::open_personalization_keys`), each
/// tried on the row — the delegable scope is kind-blinded, so a principal
/// opens the one kind it holds a key to and no other. The e2e harness's
/// principal leg over a listed `state` row.
///
/// `out` receives canonical CBOR `{"kind": bytes, "key": bytes, "value":
/// bytes}` (the first two UTF-8); a row no granted pair opens is an error.
///
/// # Safety
///
/// `grant_blob` must point to `grant_blob_len` readable bytes,
/// `holder_secret`, `writer_id` and `item_key` to 32 bytes each, `envelope` to
/// `envelope_len` bytes; `out` must be a valid, aligned, writable `FfiBuffer`
/// pointer. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)] // a flat C ABI
pub unsafe extern "C" fn fauna_personalization_open_state_row(
    grant_blob: *const u8,
    grant_blob_len: u32,
    holder_secret: *const u8,
    writer_id: *const u8,
    writer_seq: u64,
    item_key: *const u8,
    envelope: *const u8,
    envelope_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_core::account_entry_crypto::{EntryCoordinates, open_entry};
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let general = |msg: String| crate::FfiError::General { msg };
        let blob = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&slice_to_vec(
            grant_blob,
            grant_blob_len,
        ))
        .map_err(|e| general(format!("decode grant blob: {e}")))?;
        let holder = read_32(holder_secret, "holder_secret")?;
        let coords = EntryCoordinates {
            writer_id: read_32(writer_id, "writer_id")?,
            writer_seq,
            scope: fauna_protocol::account_state::ACCOUNT_STATE_SCOPE,
        };
        let item_key = read_32(item_key, "item_key")?;
        let envelope = slice_to_vec(envelope, envelope_len);
        let opened = fauna_client_capabilities::open_personalization_keys(&blob, &holder)
            .map_err(|e| general(format!("open the grant: {e}")))?
            .iter()
            .find_map(|keys| open_entry(keys, &coords, &item_key, &envelope).ok())
            .ok_or_else(|| general("no granted pair opens the row".into()))?;
        use fauna_protocol::ByteBuf;
        let reply: std::collections::BTreeMap<&str, ByteBuf> = [
            ("kind", ByteBuf::from(opened.kind.into_bytes())),
            ("key", ByteBuf::from(opened.key.into_bytes())),
            ("value", opened.value),
        ]
        .into_iter()
        .collect();
        fauna_core::encoding::canonical_encode(&reply)
            .map_err(|e| general(format!("encode opened row: {e}")))
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Open a sealed **trained-topic model** blob as its owner: the seal keys
/// derived from the owner's seed exactly as `FeedManager` derives them
/// (`model_seal_keys(BackupKey::derive(seed))`), then the production
/// `fauna_client_personalization::unseal_topic_model`. The e2e harness's
/// owner-side read of a blob it fetched off the nest — the proof that the
/// opaque bytes the nest rests are the user's own marks
/// (`topic-factors.md` § At rest — seal + home).
///
/// `out` receives canonical CBOR `{"markers": {post_id_hex: "more-like-this"
/// | "less-like-this" | <a label this build does not name>}, "example_count":
/// u32, "engagement_count": u32}` (explicit, then engagement-taught
/// documents); a blob the seed does not open is an error.
///
/// # Safety
///
/// `secret` must point to 32 readable bytes and `blob` to `blob_len`; `out`
/// must be a valid, aligned, writable `FfiBuffer` pointer. All pointers must
/// remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_personalization_open_topic_model(
    secret: *const u8,
    blob: *const u8,
    blob_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_feed::TrainVerb;
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let general = |msg: String| crate::FfiError::General { msg };
        let keys = owner_model_seal_keys(secret)?;
        let model =
            fauna_client_personalization::unseal_topic_model(&slice_to_vec(blob, blob_len), &keys)
                .map_err(|e| general(format!("open the topic model: {e}")))?;
        let markers: std::collections::BTreeMap<String, String> = model
            .example_markers()
            .map(|(id, label)| {
                let label = if label == TrainVerb::MoreLikeThis.into() {
                    "more-like-this".to_string()
                } else if label == TrainVerb::LessLikeThis.into() {
                    "less-like-this".to_string()
                } else {
                    format!("{label:?}")
                };
                (id.to_string(), label)
            })
            .collect();
        fauna_core::encoding::canonical_encode(&serde_json::json!({
            "markers": markers,
            "example_count": model.example_count(),
            "engagement_count": model.engagement_example_count(),
        }))
        .map_err(|e| general(format!("encode opened model: {e}")))
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Open a sealed **engagement-cue rollup** blob (`cues:v1`) as its owner,
/// through the production `fauna_feed::unseal_cue_rollup` under the same seal
/// keys as [`fauna_personalization_open_topic_model`]
/// (`engagement-cues.md` § At rest — the sealed cue rollup).
///
/// `out` receives canonical CBOR `{content_id: verdict}`, each verdict its wire
/// string (`"watch-complete"`, `"skip"`, or the string a newer build wrote).
///
/// # Safety
///
/// As [`fauna_personalization_open_topic_model`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_personalization_open_cue_rollup(
    secret: *const u8,
    blob: *const u8,
    blob_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_feed::CueVerdict;
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let general = |msg: String| crate::FfiError::General { msg };
        let keys = owner_model_seal_keys(secret)?;
        let rollup = fauna_feed::unseal_cue_rollup(&slice_to_vec(blob, blob_len), &keys)
            .map_err(|e| general(format!("open the cue rollup: {e}")))?;
        let items: std::collections::BTreeMap<String, String> = rollup
            .iter()
            .map(|(id, cues)| {
                let verdict = match &cues.verdict {
                    CueVerdict::Other(s) => s.clone(),
                    known => known.wire_str().unwrap_or_default().to_string(),
                };
                (id.clone(), verdict)
            })
            .collect();
        fauna_core::encoding::canonical_encode(&items)
            .map_err(|e| general(format!("encode opened rollup: {e}")))
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// The owner's personalization seal keys from their 32-byte seed — the
/// derivation `FeedManager` seals its models and cue rollup under.
///
/// # Safety
///
/// `secret` must point to 32 readable bytes.
unsafe fn owner_model_seal_keys(
    secret: *const u8,
) -> Result<fauna_core::crypto::DelegableKindKeys, crate::FfiError> {
    // SAFETY: per this function's contract.
    let seed = unsafe { read_32(secret, "secret")? };
    Ok(fauna_client_personalization::model_seal_keys(
        &fauna_core::crypto::BackupKey::derive(&seed),
    ))
}

/// Build the canonical-CBOR **consent-time folder deposit grant to a
/// third-party principal** (`folder-deposit.md`) —
/// the Python e2e twin of the deposit arm of the owner's mint inside
/// `fauna_client_capabilities::ext_consent::prepare_ext_consent_grant`, minus
/// the ledger `Mint` a harness holding no account store cannot make (pinned in
/// Rust). It runs the production steps over the consent row's own `scopes`
/// (newline-separated, as `list_pending_consents` returned them): qualify the
/// bare folder-plane scopes by the card's `folder` choice
/// (`ext_consent::approved_scopes`; `folder` 0 = no choice), collect the
/// deposit folders (`ext_consent::folder_deposit_ids`), and mint one keyless
/// `deposit` tuple per folder held by `holder_x25519`, owned by the actor
/// `identity_seed` derives. A row naming no deposit scope is refused.
///
/// # Safety
///
/// `identity_seed` (32), `grant_id` (16) and `holder_x25519` (32) must point
/// to the stated byte counts; `scopes` must be a valid NUL-terminated C
/// string; `out` must be a valid, aligned, writable `FfiBuffer` pointer. All
/// pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_capability_build_consent_deposit_grant(
    identity_seed: *const u8,
    grant_id: *const u8,
    holder_x25519: *const u8,
    epoch_start: u64,
    epoch_end: u64,
    scopes: *const c_char,
    folder: i64,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_capabilities::ext_consent::{approved_scopes, folder_deposit_ids};
    use fauna_mls::wrapped_blob::GrantWindow;
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let seed = read_32(identity_seed, "identity_seed")?;
        let grant_a: [u8; 16] =
            slice_to_vec(grant_id, 16)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "grant_id must be 16 bytes".into(),
                })?;
        let holder = read_32(holder_x25519, "holder_x25519")?;
        let scopes: Vec<String> = cstr_to_string(scopes)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        let scopes = approved_scopes(&scopes, (folder != 0).then_some(folder)).map_err(|e| {
            crate::FfiError::General {
                msg: format!("consent scopes: {e}"),
            }
        })?;
        let folders = folder_deposit_ids(&scopes);
        if folders.is_empty() {
            return Err(crate::FfiError::General {
                msg: "the consent names no folder deposit".into(),
            });
        }
        let keypair = fauna_core::identity::ActorKeypair::from_secret(seed);
        let blob = fauna_client_capabilities::mint_folder_deposit_grant(
            &keypair.actor_id().0,
            &grant_a,
            &holder,
            GrantWindow(epoch_start, epoch_end),
            &folders,
        )
        .map_err(|e| crate::FfiError::General {
            msg: format!("mint folder deposit grant: {e}"),
        })?;
        blob.to_canonical_bytes()
            .map_err(|e| crate::FfiError::General {
                msg: format!("encode grant blob: {e}"),
            })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Open a **principal's folder read twin** (`webdav-server.md` § Key model →
/// *A principal's read*) with the holder's X25519 secret into the value of
/// the `Fauna-Folder-Keys` request header its WebDAV bearer requests carry —
/// the production `fauna_client_capabilities::open_folder_read_keys`, then
/// `FolderContentKeys::to_header_value`. The e2e harness's principal leg over
/// a grant it fetched with `fauna.capabilities.fetch`; a blob that is no
/// folder twin, or that the key does not open, is an error.
///
/// `out` receives the header value's UTF-8 bytes.
///
/// # Safety
///
/// `grant_blob` must point to `grant_blob_len` readable bytes and
/// `holder_secret` to 32; `out` must be a valid, aligned, writable
/// `FfiBuffer` pointer. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_folder_read_keys_header(
    grant_blob: *const u8,
    grant_blob_len: u32,
    holder_secret: *const u8,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let general = |msg: String| crate::FfiError::General { msg };
        let blob = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&slice_to_vec(
            grant_blob,
            grant_blob_len,
        ))
        .map_err(|e| general(format!("decode grant blob: {e}")))?;
        let holder = read_32(holder_secret, "holder_secret")?;
        fauna_client_capabilities::open_folder_read_keys(&blob, &holder)
            .map_err(|e| general(format!("open the folder twin: {e}")))?
            .to_header_value()
            .map(String::into_bytes)
            .map_err(|e| general(format!("encode the keys header: {e:#}")))
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Build the canonical-CBOR **keyless `identity.op` grant to a third-party
/// principal** — one tuple naming the operation class `class_name`, held by
/// `holder_x25519` (`key-material-hierarchy.md` § Audience: deployment
/// infrastructure → *The oracle*). The Python e2e twin of the owner's mint:
/// `fauna_client_capabilities::identity_op_scope` over the shared grant
/// builder, owned by the actor `identity_seed` derives. A sovereign or
/// unknown class name is refused, as the mint refuses it.
///
/// # Safety
///
/// `identity_seed` (32), `grant_id` (16) and `holder_x25519` (32) must point
/// to the stated byte counts; `class_name` must be a valid NUL-terminated C
/// string; `out` must be a valid, aligned, writable `FfiBuffer` pointer. All
/// pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_capability_build_identity_op_grant(
    identity_seed: *const u8,
    grant_id: *const u8,
    holder_x25519: *const u8,
    epoch_start: u64,
    epoch_end: u64,
    class_name: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_mls::wrapped_blob::GrantWindow;
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let seed = read_32(identity_seed, "identity_seed")?;
        let grant_a: [u8; 16] =
            slice_to_vec(grant_id, 16)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "grant_id must be 16 bytes".into(),
                })?;
        let holder = read_32(holder_x25519, "holder_x25519")?;
        let name = cstr_to_string(class_name)?;
        let class = fauna_core::identity_op::IdentityOpClass::parse(&name).map_err(|e| {
            crate::FfiError::General {
                msg: format!("identity.op class {name:?}: {e}"),
            }
        })?;
        let keypair = fauna_core::identity::ActorKeypair::from_secret(seed);
        let blob = fauna_mls::wrapped_blob::build_grant_blob(
            &keypair.actor_id().0,
            &grant_a,
            &holder,
            None,
            GrantWindow(epoch_start, epoch_end),
            &[(fauna_client_capabilities::identity_op_scope(class), None)],
        )
        .map_err(|e| crate::FfiError::General {
            msg: format!("build identity.op grant: {e}"),
        })?;
        blob.to_canonical_bytes()
            .map_err(|e| crate::FfiError::General {
                msg: format!("encode grant blob: {e}"),
            })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Seal one `ext.*` row **as a third-party principal**: open the kind's pair
/// out of `grant_blob` with the holder's X25519 secret (the production
/// `fauna_client_capabilities::open_ext_kind_keys`), then seal a `latest-wins`
/// entry under `ext:<kind>` signed by the principal's Ed25519 writer key
/// (`fauna_core::account_entry_crypto::seal_entry`, the stamp an admitted
/// `ext.*` kind's merge reads). What a connected app does before its
/// `fauna.account.state.put`; the e2e harness's principal leg. A non-zero
/// `tombstone` seals the deletion of `key` instead — the payload's tombstone
/// marker, the entry a record door's `DELETE` carries.
///
/// `out` receives canonical CBOR `{"item_key": bytes(32), "envelope": bytes}`.
///
/// # Safety
///
/// `grant_blob` must point to `grant_blob_len` readable bytes; `holder_secret`
/// and `writer_secret` to 32 bytes each; `kind` and `key` must be valid
/// NUL-terminated C strings; `value` must point to `value_len` readable bytes
/// (or be null with len 0); `out` must be a valid, aligned, writable
/// `FfiBuffer` pointer. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)] // a flat C ABI
pub unsafe extern "C" fn fauna_ext_kind_seal_row(
    grant_blob: *const u8,
    grant_blob_len: u32,
    holder_secret: *const u8,
    writer_secret: *const u8,
    kind: *const c_char,
    key: *const c_char,
    value: *const u8,
    value_len: u32,
    writer_seq: u64,
    at_ms: i64,
    tombstone: u8,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_core::account_entry_crypto::{EntryCoordinates, EntryPlaintext, seal_entry};
    use fauna_protocol::ByteBuf;
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let general = |msg: String| crate::FfiError::General { msg };
        let blob = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&slice_to_vec(
            grant_blob,
            grant_blob_len,
        ))
        .map_err(|e| general(format!("decode grant blob: {e}")))?;
        let holder = read_32(holder_secret, "holder_secret")?;
        let writer =
            ed25519_dalek::SigningKey::from_bytes(&read_32(writer_secret, "writer_secret")?);
        let kind_s = cstr_to_string(kind)?;
        let ext_kind: fauna_protocol::ext_kind::ExtKind = kind_s
            .parse()
            .map_err(|e| general(format!("kind {kind_s:?}: {e}")))?;
        let keys = fauna_client_capabilities::open_ext_kind_keys(&blob, &holder)
            .map_err(|e| general(format!("open the grant: {e}")))?
            .into_iter()
            .find(|k| k.kind() == kind_s)
            .ok_or_else(|| general(format!("the grant carries no pair for {kind_s}")))?;
        let writer_id = writer.verifying_key().to_bytes();
        let stamp = fauna_protocol::merge_policy::LwwStamp {
            at_ms,
            writer: writer_id,
        }
        .encode()
        .map_err(|e| general(format!("encode stamp: {e}")))?;
        let scope = fauna_protocol::scope::ext_scope(&ext_kind);
        let sealed = seal_entry(
            &keys,
            &EntryCoordinates {
                writer_id,
                writer_seq,
                scope: &scope,
            },
            &EntryPlaintext {
                kind: kind_s.clone(),
                key: cstr_to_string(key)?,
                merge_meta: Some(ByteBuf::from(stamp)),
                value: ByteBuf::from(slice_to_vec(value, value_len)),
                tombstone: tombstone != 0,
            },
            &writer,
        )
        .map_err(|e| general(format!("seal: {e:#}")))?;
        let reply: std::collections::BTreeMap<&str, ByteBuf> = [
            ("item_key", ByteBuf::from(sealed.item_key.to_vec())),
            ("envelope", ByteBuf::from(sealed.envelope)),
        ]
        .into_iter()
        .collect();
        fauna_core::encoding::canonical_encode(&reply)
            .map_err(|e| general(format!("encode sealed row: {e}")))
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Seal one bridged message **as a bridge principal**: to the user's
/// registered recipient key, the way a bridge seals a
/// `fauna.bridges.conversation.deposit` after reading that key with
/// `fauna.bridges.fetch_recipient_mls_pubkey` — hybrid (X-Wing) when the
/// account published an ML-KEM encapsulation key, classical X25519 otherwise.
/// The e2e harness's bridge leg; `out` receives the canonical envelope bytes.
///
/// # Safety
///
/// `recipient_x25519` must point to 32 readable bytes; `mlkem_ek` to
/// `mlkem_ek_len` readable bytes (or be null with len 0); `plaintext` to
/// `plaintext_len` readable bytes (or be null with len 0); `out` must be a
/// valid, aligned, writable `FfiBuffer` pointer. All pointers must remain
/// valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_bridged_seal_for_user(
    recipient_x25519: *const u8,
    mlkem_ek: *const u8,
    mlkem_ek_len: u32,
    plaintext: *const u8,
    plaintext_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_mls::wrapped_blob::{XWingPublicKey, seal_to_recipient, seal_to_recipient_xwing};
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let general = |msg: String| crate::FfiError::General { msg };
        let x25519 = read_32(recipient_x25519, "recipient_x25519")?;
        let ek = slice_to_vec(mlkem_ek, mlkem_ek_len);
        let body = slice_to_vec(plaintext, plaintext_len);
        let envelope = if ek.is_empty() {
            seal_to_recipient(&body, &x25519)
        } else {
            // An ML-KEM-768 encapsulation key (FIPS 203): 1184 bytes.
            let ek: [u8; 1184] = ek
                .as_slice()
                .try_into()
                .map_err(|_| general("mlkem_ek is not an ML-KEM-768 encapsulation key".into()))?;
            seal_to_recipient_xwing(&body, &XWingPublicKey::from_parts(ek, x25519))
        }
        .map_err(|e| general(format!("seal: {e}")))?;
        envelope
            .to_canonical_bytes()
            .map_err(|e| general(format!("encode envelope: {e}")))
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Open one outbound bridged item **as the bridge principal**: an item the
/// user's app sealed to the principal's X25519 holder key, drained with
/// `fauna.bridges.conversation.outbox.fetch`. The e2e harness's bridge leg —
/// and its proof that the item opens under the bridge's key; `out` receives
/// the plaintext.
///
/// # Safety
///
/// `bridge_secret` must point to 32 readable bytes; `sealed` to `sealed_len`
/// readable bytes; `out` must be a valid, aligned, writable `FfiBuffer`
/// pointer. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_bridged_open_as_bridge(
    bridge_secret: *const u8,
    sealed: *const u8,
    sealed_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_mls::wrapped_blob::{MailRecordEnvelope, unseal_mail_record};
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `read_32`/`slice_to_vec`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let general = |msg: String| crate::FfiError::General { msg };
        let secret = read_32(bridge_secret, "bridge_secret")?;
        let envelope = MailRecordEnvelope::from_canonical_bytes(&slice_to_vec(sealed, sealed_len))
            .map_err(|e| general(format!("decode envelope: {e}")))?;
        unseal_mail_record(&envelope, &secret).map_err(|e| general(format!("open: {e}")))
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// BARE-encode a signed feed `Post`, optionally tagged.
///
/// `secret` / `secret_len` — 32-byte Ed25519 secret.
/// `body` — NUL-terminated UTF-8 post body.
/// `tags_json` — null, or a NUL-terminated UTF-8 JSON array of tag strings
///               (e.g. `["test","foo"]`, without the `#`); null / empty /
///               `"[]"` ⇒ no tags. A C-ABI string array is fiddly and the
///               test helper only needs a handful of tags, so a single
///               JSON-array `c_char_p` keeps the ABI to one pointer.
/// `out` — receives an allocated buffer; caller frees via `fauna_ffi_free_buffer`.
///
/// Builds a feed post — lets the Python E2E
/// helper produce the `fauna.posts.create` body via the shared
/// `fauna_client_core::post::build_post` builder, replacing the deleted
/// `POST /api/v1/feeds/{id}/posts` REST twin.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes (or be null with
/// `secret_len` 0); `body` must be a valid NUL-terminated C string;
/// `tags_json` must be null or a valid NUL-terminated C string; `reply_to`
/// must be null with `reply_to_len` 0, or point to `reply_to_len` (= 36)
/// readable bytes; `out` must be a valid, aligned, writable pointer to an
/// `FfiBuffer`. All pointers must remain valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_post_build(
    secret: *const u8,
    secret_len: u32,
    body: *const c_char,
    tags_json: *const c_char,
    reply_to: *const u8,
    reply_to_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `secret`/`secret_len`, `body`, and `tags_json` meet
    // `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let kp = crate::keypair_from_bytes(&secret_vec)?;
        let body_s = cstr_to_string(body)?;
        let tags: Vec<String> = if tags_json.is_null() {
            Vec::new()
        } else {
            let s = cstr_to_string(tags_json)?;
            if s.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&s).map_err(|e| crate::FfiError::General {
                    msg: format!("tags_json must be a JSON array of strings: {e}"),
                })?
            }
        };
        let reply: Option<[u8; 36]> = if reply_to.is_null() || reply_to_len == 0 {
            None
        } else {
            Some(
                slice_to_vec(reply_to, reply_to_len)
                    .try_into()
                    .map_err(|v: Vec<u8>| crate::FfiError::General {
                        msg: format!("reply_to must be a 36-byte CID, got {}", v.len()),
                    })?,
            )
        };
        fauna_client_core::post::build_post(&kp, &body_s, &tags, reply)
            .map_err(|e| crate::FfiError::General { msg: e.0 })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Build + sign a feed Post with image attachments
/// (`PostBody::TextWithMedia`) and return the `fauna.posts.create` body.
///
/// `media_json` — a NUL-terminated UTF-8 JSON array of media descriptors:
/// `[{"blob_cid":"b…","mime":"image/png","size_bytes":123,"width":800,"height":600}]`.
/// `blob_cid` is the Fauna content CID of bytes the caller has ALREADY uploaded
/// (`PUT /api/v1/blob/{cid_b32}`) — the post is a signed claim about those
/// bytes, so building one for a blob the nest does not hold would be a lie the
/// signature carries. `width`/`height` are optional; both must be present for
/// dimensions to be recorded.
///
/// The e2e twin of the shared `fauna_client_core::post::build_post_with_media`
/// builder — the media half of [`fauna_post_build`], and what lets the ATProto
/// projection's image path be driven end to end from Python.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes (or be null with
/// `secret_len` 0); `body` and `media_json` must be valid NUL-terminated C
/// strings; `out` must be a valid, aligned, writable pointer to an `FfiBuffer`.
/// All pointers must remain valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_post_build_with_media(
    secret: *const u8,
    secret_len: u32,
    body: *const c_char,
    media_json: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `secret`/`secret_len`, `body` and `media_json` meet
    // `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let kp = crate::keypair_from_bytes(&secret_vec)?;
        let body_s = cstr_to_string(body)?;
        // Parsed through `serde_json::Value` rather than a derived struct:
        // fauna-ffi does not depend on `serde` itself (only `serde_json`), and
        // this descriptor is small enough that reading it field by field costs
        // less than a new dependency on the whole client FFI surface.
        let descs = {
            let s = cstr_to_string(media_json)?;
            let v: serde_json::Value =
                serde_json::from_str(&s).map_err(|e| crate::FfiError::General {
                    msg: format!("media_json must be JSON: {e}"),
                })?;
            match v {
                serde_json::Value::Array(items) => items,
                _ => {
                    return Err(crate::FfiError::General {
                        msg: "media_json must be a JSON ARRAY of media descriptors".to_string(),
                    });
                }
            }
        };
        let bad = |field: &str| crate::FfiError::General {
            msg: format!("media descriptor is missing or has a non-conforming {field}"),
        };
        let items = descs
            .into_iter()
            .map(|d| {
                let blob_cid = d
                    .get("blob_cid")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| bad("blob_cid"))?;
                let mime = d
                    .get("mime")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| bad("mime"))?;
                let size_bytes = d
                    .get("size_bytes")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| bad("size_bytes"))?;
                let dim = |field: &str| d.get(field).and_then(|v| v.as_u64()).map(|n| n as u32);
                let blob_hash =
                    fauna_core::data::ContentHash::from_base32(blob_cid).map_err(|e| {
                        crate::FfiError::General {
                            msg: format!("media blob_cid {blob_cid:?} is not a CID: {e}"),
                        }
                    })?;
                Ok(fauna_core::data::MediaItem {
                    blob_hash,
                    media_type: mime.to_string(),
                    size_bytes,
                    dimensions: match (dim("width"), dim("height")) {
                        (Some(width), Some(height)) => {
                            Some(fauna_core::data::Dimensions { width, height })
                        }
                        _ => None,
                    },
                    thumbnail: None,
                    remote_url: None,
                    alt: None,
                })
            })
            .collect::<Result<Vec<_>, crate::FfiError>>()?;
        fauna_client_core::post::build_post_with_media(&kp, &body_s, items, &[], None)
            .map_err(|e| crate::FfiError::General { msg: e.0 })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Build + sign a video Post (`PostBody::Video`) and return the
/// `fauna.posts.create` body.
///
/// `video_json` — a NUL-terminated UTF-8 JSON object:
/// `{"manifest_cid":"b…","thumbnail_cid":"b…","duration_ms":12000,
///   "aspect_width":16,"aspect_height":9,
///   "segments":[{"blob_cid":"b…","resolution":720,"codec":"h264",
///                "bitrate":2500,"byte_size":1234}]}`.
/// Every `blob_cid` names bytes the caller has ALREADY uploaded
/// (`PUT /api/v1/blob/{cid_b32}`) — the same signed-claim rule as
/// [`fauna_post_build_with_media`]. `segments` is in PLAYBACK ORDER within each
/// resolution, which is what the ATProto projection concatenates.
///
/// The e2e twin of `fauna_client_core::post::build_video_post`, and what lets
/// the projection's video path be driven end to end from Python.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes (or be null with
/// `secret_len` 0); `video_json` must be a valid NUL-terminated C string; `out`
/// must be a valid, aligned, writable pointer to an `FfiBuffer`. All pointers
/// must remain valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_post_build_video(
    secret: *const u8,
    secret_len: u32,
    video_json: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `secret`/`secret_len` and `video_json` meet
    // `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let kp = crate::keypair_from_bytes(&secret_vec)?;
        let v: serde_json::Value = {
            let s = cstr_to_string(video_json)?;
            serde_json::from_str(&s).map_err(|e| crate::FfiError::General {
                msg: format!("video_json must be JSON: {e}"),
            })?
        };
        let bad = |field: &str| crate::FfiError::General {
            msg: format!("video descriptor is missing or has a non-conforming {field}"),
        };
        let cid = |field: &str| -> Result<fauna_core::data::ContentHash, crate::FfiError> {
            let s = v
                .get(field)
                .and_then(|x| x.as_str())
                .ok_or_else(|| bad(field))?;
            fauna_core::data::ContentHash::from_base32(s).map_err(|e| crate::FfiError::General {
                msg: format!("video {field} {s:?} is not a CID: {e}"),
            })
        };
        let num = |field: &str| {
            v.get(field)
                .and_then(|x| x.as_u64())
                .ok_or_else(|| bad(field))
        };

        let segments = match v.get("segments") {
            Some(serde_json::Value::Array(items)) => items.clone(),
            _ => return Err(bad("segments")),
        };
        let segments = segments
            .into_iter()
            .map(|s| {
                let get = |field: &str| {
                    s.get(field)
                        .and_then(|x| x.as_u64())
                        .ok_or_else(|| bad(field))
                };
                let blob_cid = s
                    .get("blob_cid")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| bad("segment blob_cid"))?;
                Ok(fauna_core::data::VideoSegment {
                    hash: fauna_core::data::ContentHash::from_base32(blob_cid).map_err(|e| {
                        crate::FfiError::General {
                            msg: format!("segment blob_cid {blob_cid:?} is not a CID: {e}"),
                        }
                    })?,
                    resolution: get("resolution")? as u16,
                    codec: s
                        .get("codec")
                        .and_then(|x| x.as_str())
                        .unwrap_or("h264")
                        .to_string(),
                    bitrate: get("bitrate").unwrap_or(2500) as u32,
                    byte_size: get("byte_size")?,
                })
            })
            .collect::<Result<Vec<_>, crate::FfiError>>()?;

        fauna_client_core::post::build_video_post(
            &kp,
            cid("manifest_cid")?,
            segments,
            cid("thumbnail_cid")?,
            num("duration_ms")?,
            (num("aspect_width")? as u16, num("aspect_height")? as u16),
        )
        .map_err(|e| crate::FfiError::General { msg: e.0 })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Build + sign a `Profile` with optional pictures and return the
/// `fauna.profile.set` body (the signed `EmbedAsBytes` wire).
///
/// `display_name` / `bio` — null or a NUL-terminated UTF-8 string; null omits
/// the field, exactly as a profile that never set it.
/// `avatar_cid` / `banner_cid` — null, or the base32 Fauna content CID of
/// bytes the caller has ALREADY uploaded (`PUT /api/v1/blob/{cid_b32}`). Same
/// rule as [`fauna_post_build_with_media`]'s descriptors: the profile is a
/// signed claim about those bytes, so naming a blob the nest does not hold
/// would be a lie the signature carries.
///
/// The e2e twin of `fauna_client_profile::build_profile`, which it calls
/// rather than re-implementing the sign step — what lets the ATProto
/// projection's avatar/banner path be driven end to end from Python.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes (or be null with
/// `secret_len` 0); each of `display_name`, `bio`, `avatar_cid`, `banner_cid`
/// must be null or a valid NUL-terminated C string; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`. All pointers must remain valid
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_profile_build_with_pictures(
    secret: *const u8,
    secret_len: u32,
    display_name: *const c_char,
    bio: *const c_char,
    avatar_cid: *const c_char,
    banner_cid: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let kp = crate::keypair_from_bytes(&secret_vec)?;
        let opt_str = |p: *const c_char| -> Result<Option<String>, crate::FfiError> {
            if p.is_null() {
                Ok(None)
            } else {
                Ok(Some(cstr_to_string(p)?))
            }
        };
        let opt_hash =
            |p: *const c_char| -> Result<Option<fauna_core::data::ContentHash>, crate::FfiError> {
                match opt_str(p)? {
                    None => Ok(None),
                    Some(cid) => fauna_core::data::ContentHash::from_base32(&cid)
                        .map(Some)
                        .map_err(|e| crate::FfiError::General {
                            msg: format!("picture cid {cid:?} is not a CID: {e}"),
                        }),
                }
            };
        let profile = fauna_core::data::Profile {
            actor_id: kp.actor_id(),
            display_name: opt_str(display_name)?,
            bio: opt_str(bio)?,
            avatar: opt_hash(avatar_cid)?,
            banner: opt_hash(banner_cid)?,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: fauna_core::data::InboxMode::Open,
            recovery_head: None,
            audience: fauna_core::data::ProfileAudience::ThisNest,
            updated_at: fauna_core::data::Timestamp(0),
        };
        fauna_client_profile::build_profile(&kp, &profile).map_err(|e| crate::FfiError::General {
            msg: format!("build profile: {e}"),
        })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Mint a D10 delegated-authoring cert — the embed-as-bytes wire that
/// `fauna.bridges.atproto.provision_authoring_delegation` carries.
///
/// `secret` / `secret_len` — the account's 32-byte Ed25519 **identity**
///                           secret. It signs the cert, and its public half
///                           becomes the grantor.
/// `k_pub` / `k_pub_len` — the 32-byte authoring sub-key from
///                         `fauna.bridges.atproto.fetch_authoring_key`.
/// `capabilities_json` — null, or a NUL-terminated UTF-8 JSON array of
///                       capability names (`["Post"]`, `["Post",
///                       "UpdateProfile"]`); null / empty ⇒ `["Post"]`, the
///                       hosted-enable default. Same single-`c_char_p`
///                       convention `fauna_post_build`'s `tags_json` uses.
/// `created_at_micros` — epoch MICROSECONDS the cert is created at
///                       (`fauna_core::data::Timestamp`'s unit).
/// `expires_at_micros` — epoch MICROSECONDS the cert expires at, or 0 for a
///                       non-expiring delegation (revocable from the client
///                       regardless — revoking destroys `K` itself). Passing
///                       milliseconds here mints a cert that reads as decades
///                       expired to the ingest gate's step-5 check, which
///                       compares it against the post's microsecond
///                       `created_at`.
/// `out` — receives an allocated buffer; caller frees via `fauna_ffi_free_buffer`.
///
/// Wraps the shared `fauna_client_bridges::atproto_delegation` minter that all
/// 7 apps use, so the Python E2E helper produces a byte-identical cert to a
/// real client's — the point of a tier_3 write test is that nothing on the
/// path is a test-only reimplementation.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes (or be null with
/// `secret_len` 0); likewise `k_pub` / `k_pub_len`; `capabilities_json` must
/// be null or a valid NUL-terminated C string; `out` must be a valid, aligned,
/// writable pointer to an `FfiBuffer`. All pointers must remain valid for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_atproto_delegation_cert_build(
    secret: *const u8,
    secret_len: u32,
    k_pub: *const u8,
    k_pub_len: u32,
    capabilities_json: *const c_char,
    created_at_micros: u64,
    expires_at_micros: u64,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_bridges::atproto_delegation::build_authoring_delegation_cert;
    use fauna_core::data::{Capability, Timestamp};

    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `secret`/`secret_len`, `k_pub`/`k_pub_len` and `capabilities_json` meet
    // `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let kp = crate::keypair_from_bytes(&secret_vec)?;
        let k_pub_arr: [u8; 32] =
            slice_to_vec(k_pub, k_pub_len)
                .try_into()
                .map_err(|v: Vec<u8>| crate::FfiError::General {
                    msg: format!("k_pub must be 32 bytes, got {}", v.len()),
                })?;
        let names: Vec<String> = if capabilities_json.is_null() {
            Vec::new()
        } else {
            let s = cstr_to_string(capabilities_json)?;
            if s.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&s).map_err(|e| crate::FfiError::General {
                    msg: format!("capabilities_json must be a JSON array of strings: {e}"),
                })?
            }
        };
        let caps = if names.is_empty() {
            vec![Capability::Post]
        } else {
            names
                .iter()
                .map(|n| match n.as_str() {
                    "Post" => Ok(Capability::Post),
                    "UpdateProfile" => Ok(Capability::UpdateProfile),
                    // Every other variant is outside the authoring set the
                    // nest accepts, so name the rejection here rather than
                    // letting it surface as an opaque provisioning refusal.
                    other => Err(crate::FfiError::General {
                        msg: format!(
                            "capability {other:?} is not an authoring capability (Post, UpdateProfile)"
                        ),
                    }),
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        build_authoring_delegation_cert(
            &kp,
            k_pub_arr,
            &caps,
            Timestamp(created_at_micros),
            (expires_at_micros != 0).then_some(Timestamp(expires_at_micros)),
        )
        .map_err(crate::general_err)
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Seal a **folder-relative path** under the owner root, exactly as an
/// unbound set's writer does — the label-plane analogue of
/// `fauna_folder_seal_file`, for the Python seeding seams.
///
/// **Why this export exists.** After the S9 flip a `fauna.sync.changes.record`
/// on a sealed plane rests *only* `path_sealed`; the plaintext `path` column is
/// gone. A reader that cannot open the seal therefore gets no plaintext to fall
/// back on and the row degrades to
/// [`fauna_core::path_crypto::SealedLabelRender::Omit`] — which **drops the item
/// entirely** (`libs/fauna-media-machine/src/machine.rs`), not merely renders it
/// nameless. So a raw-RPC seed carrying a *synthetic* envelope seeds rows no app
/// can ever see, and every UI assertion over them fails for a reason that has
/// nothing to do with what it is testing. This hands Python the **real** funnel
/// instead: the bytes come from `fauna_core::label_custody::seal_path`, so the
/// seed cannot drift from what a client writes. Reimplementing the derivation
/// in Python would be a fourth writer of a plane whose whole design rule is one
/// funnel (`file-sync.md` § Sealed names & paths), and a drifted seed fails
/// **silently** — as `Omit`, never as an error.
///
/// **Root choice is the unbound-set arm, deliberately.** `LabelRoot::owner` over
/// `BackupKey::derive(seed).convergent_chunk_root()`, stamping no generation —
/// arm-for-arm what `FileDownloadKeys::label_seal_root` picks for a set with no
/// M2 content keys, which is what a set created by `fauna.folders.create` and
/// never bound to a folder is. A set that *is* bound seals under its content-key
/// generation instead, and this export must not be used for one: the envelope's
/// `gen` is what tells the reader which root to try, so an owner-rooted blob on
/// a bound set opens for nobody.
///
/// `secret` / `secret_len` — the actor's 32-byte Ed25519 seed (the same bytes
/// the session's `secret_hex` carries).
/// `path` — NUL-terminated UTF-8, folder-relative, forward-slash normalized.
/// `out` — receives the canonical dag-cbor `SealedLabel` envelope; caller frees
/// via `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes (or be null with
/// `secret_len` 0); `path` must be a valid NUL-terminated C string; `out` must
/// be a valid, aligned, writable pointer to an `FfiBuffer`. All pointers must
/// remain valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_path_seal(
    secret: *const u8,
    secret_len: u32,
    path: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `secret`/`secret_len` and `path` meet `slice_to_vec`/`cstr_to_string`'s
    // preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let seed: [u8; 32] =
            secret_vec
                .as_slice()
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: format!(
                        "secret must be exactly 32 bytes (the Ed25519 seed), got {}",
                        secret_vec.len()
                    ),
                })?;
        let path_s = cstr_to_string(path)?;
        let key = fauna_core::crypto::BackupKey::derive(&seed);
        let root = fauna_core::path_crypto::LabelRoot::owner_of(&key);
        fauna_core::label_custody::seal_path(&root, &path_s).map_err(|e| crate::FfiError::General {
            msg: format!("seal path: {e}"),
        })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Seal a **user-chosen device label** under the registering owner's root —
/// the device-plane twin of [`fauna_path_seal`], for the Python seeding seams.
///
/// **Why this export exists.** The S9 flip added a plaintext scrub to the nest's
/// device writer (`register_sync_device`, `bins/fauna-nest/src/db/sync_storage.rs`):
/// a user-chosen label rests **sealed-only**, and a *sealless* `fauna.sync.register`
/// therefore rests no label at all. Every e2e device seed posted
/// `{device_id, label, capabilities}` with no `label_sealed`, so every seeded row
/// came back nameless and every device-name assertion failed for a reason
/// unrelated to what it tests. This hands Python the **real** funnel
/// ([`fauna_core::label_custody::seal_device_label`]) rather than letting a
/// fixture reimplement the derivation — the same one-funnel rule
/// [`fauna_path_seal`] exists for, and with the same silent failure mode: a
/// drifted seal degrades to [`fauna_core::path_crypto::SealedLabelRender::Omit`],
/// never to an error.
///
/// ⚠ **The degrade is plane-specific, and this plane's is the weaker one.**
/// `DevicesMachine::render_devices` maps `Omit` to an **empty label on a kept
/// row** (a device is actionable by `device_id` alone and hiding one the user
/// may need to revoke is the worse outcome), where the path/media plane *drops*
/// the item. So a drifted device seal breaks only *name* assertions; counts stay
/// correct. Do not carry a conclusion from one plane to the other
/// (`file-sync.md` § Sealed names & paths).
///
/// **Root is the registering owner's, salt is the raw `device_id`** — never a
/// folder's M2 generation. A device belongs to the *actor*, not to any set;
/// this is arm-for-arm what `fauna-sync-engine`'s `register_device` does with
/// `owner_backup_key(&credential)`.
///
/// **An empty `out` buffer is success, not failure.** `seal_device_label`
/// returns `Ok(None)` for the three machine-authored labels
/// (`is_synthetic_device_label` — the WebDAV pseudo-device, the backup
/// coordinator, the self-register placeholder), which rest plaintext by ratified
/// design and must **not** seal. That arm is reported as rc `0` with `len == 0`,
/// mirroring the Rust `Option`, so a caller passing a synthetic label registers
/// sealless exactly as a production writer does.
///
/// `secret` / `secret_len` — the actor's 32-byte Ed25519 seed (the same bytes
/// the session's `secret_hex` carries).
/// `device_id` / `device_id_len` — the raw 32-byte device id (the seal's salt);
/// hex-decode the `fauna.sync.register` spelling before calling.
/// `label` — NUL-terminated UTF-8, the user-chosen label.
/// `out` — receives the canonical dag-cbor `SealedLabel` envelope, or an empty
/// buffer for a synthetic label; caller frees via `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes and `device_id` to
/// `device_id_len` readable bytes (or either may be null with its len 0);
/// `label` must be a valid NUL-terminated C string; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`. All pointers must remain valid
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_device_label_seal(
    secret: *const u8,
    secret_len: u32,
    device_id: *const u8,
    device_id_len: u32,
    label: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `secret`/`secret_len`, `device_id`/`device_id_len` and `label` meet
    // `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let seed: [u8; 32] =
            secret_vec
                .as_slice()
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: format!(
                        "secret must be exactly 32 bytes (the Ed25519 seed), got {}",
                        secret_vec.len()
                    ),
                })?;
        let device_vec = slice_to_vec(device_id, device_id_len);
        let device: [u8; 32] =
            device_vec
                .as_slice()
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: format!(
                        "device_id must be exactly 32 bytes, got {}",
                        device_vec.len()
                    ),
                })?;
        let label_s = cstr_to_string(label)?;
        let key = fauna_core::crypto::BackupKey::derive(&seed);
        let root = fauna_core::path_crypto::LabelRoot::owner_of(&key);
        fauna_core::label_custody::seal_device_label(&root, &device, &label_s)
            // `Ok(None)` — a machine-authored label, which must rest plaintext.
            // Reported as an empty buffer, not an error: registering it sealless
            // is what a production writer does.
            .map(|sealed| sealed.unwrap_or_default())
            .map_err(|e| crate::FfiError::General {
                msg: format!("seal device label: {e}"),
            })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Render one device label sealed-first, exactly as an app's device list does —
/// the read-side twin of [`fauna_device_label_seal`].
///
/// **Why an *open* and not a recompute.** The path plane's seal is convergent, so
/// a test asserts on it by re-sealing the path it expects and comparing bytes
/// (`common.sealed_path`). A device label seals with a **random nonce** (it is
/// mutable and its salt does not determine it), so its envelope is not
/// reproducible and that trick is unavailable: opening is the only way a
/// driver-less test can assert the label a register actually stored. This is the
/// same `fauna_core::label_custody::render_device_label` seam
/// `DevicesMachine::render_devices` calls, under the same owner-only custody, so
/// what this renders is by construction what the owner's app renders.
///
/// **An empty `out` buffer means [`fauna_core::path_crypto::SealedLabelRender::Omit`]** —
/// this reader cannot render the label. On *this* plane that is the ratified
/// degrade rather than a hard failure: an app keeps the row and shows an empty
/// name, because a device stays actionable by `device_id`. It is also
/// indistinguishable from a genuinely empty label, which no user-chosen label is.
///
/// `secret` / `secret_len` — the actor's 32-byte Ed25519 seed.
/// `device_id` / `device_id_len` — the raw 32-byte device id (the seal's salt).
/// `sealed` / `sealed_len` — the `label_sealed` envelope off the wire row; null
/// with len 0 for a row that carries none.
/// `plaintext` — the row's `label` column, NUL-terminated UTF-8 (`""` post-flip
/// for a user-chosen label; the sealed-first render falls back to it only while
/// it is non-empty, which is what keeps the three synthetic labels renderable).
/// `out` — receives the rendered label as UTF-8, empty for `Omit`; caller frees
/// via `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret`, `device_id` and `sealed` must each point to their stated number of
/// readable bytes (or be null with len 0); `plaintext` must be a valid
/// NUL-terminated C string; `out` must be a valid, aligned, writable pointer to
/// an `FfiBuffer`. All pointers must remain valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_device_label_open(
    secret: *const u8,
    secret_len: u32,
    device_id: *const u8,
    device_id_len: u32,
    sealed: *const u8,
    sealed_len: u32,
    plaintext: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer/len pair meets `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let seed: [u8; 32] =
            secret_vec
                .as_slice()
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: format!(
                        "secret must be exactly 32 bytes (the Ed25519 seed), got {}",
                        secret_vec.len()
                    ),
                })?;
        let device_vec = slice_to_vec(device_id, device_id_len);
        let device: [u8; 32] =
            device_vec
                .as_slice()
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: format!(
                        "device_id must be exactly 32 bytes, got {}",
                        device_vec.len()
                    ),
                })?;
        let sealed_vec = slice_to_vec(sealed, sealed_len);
        let plaintext_s = cstr_to_string(plaintext)?;
        let keys = fauna_core::file_download::FileDownloadKeys::owner(
            fauna_core::crypto::BackupKey::derive(&seed),
        );
        let rendered = fauna_core::label_custody::render_device_label(
            &keys,
            (!sealed_vec.is_empty()).then_some(sealed_vec.as_slice()),
            &plaintext_s,
            &device,
        );
        Ok::<_, crate::FfiError>(
            rendered
                .text()
                .map(|s| s.as_bytes().to_vec())
                .unwrap_or_default(),
        )
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Sign a device's enrollment grant — the root-signed
/// `[RenewBearer, SyncWrite]` `DeviceAuthorization` over `device_key` — for the Python seeding seams, so a
/// fixture can give a `fauna.sync.register` row a **granted principal** through
/// `fauna.sync.device_grant.register`, as a real enrollment does.
///
/// **Why this export exists.** A sealless fixture register leaves the row's
/// `principal` empty, and every principal-keyed rule on the Devices page treats
/// such a row as principal-less and renders nothing: the keyless-posture marker
/// (`devices.md` § Custody facet piece 1) and the fleet-removal refusals for a
/// principal that is no verified member (`devices.md` § Errors & edge cases).
/// A granted row whose key never joined the fleet is exactly the state both
/// rules exist for, and the grant must come from the one real minter
/// ([`fauna_client_sync::build_principal_grant`]) rather than a fixture's
/// reimplementation of the signed envelope.
///
/// `secret` / `secret_len` — the actor's 32-byte Ed25519 seed (the signer).
/// `device_key` / `device_key_len` — the 32-byte public key the grant names.
/// `out` — receives the canonical dag-cbor `EmbedAsBytes` (`{envelope, bytes}`),
/// the `authorization` field of the register request; caller frees via
/// `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` and `device_key` must each point to their stated number of readable
/// bytes (or be null with len 0); `out` must be a valid, aligned, writable
/// pointer to an `FfiBuffer`. All pointers must remain valid for the duration of
/// the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_device_grant_build(
    secret: *const u8,
    secret_len: u32,
    device_key: *const u8,
    device_key_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `secret`/`secret_len` and `device_key`/`device_key_len` meet
    // `slice_to_vec`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let seed: [u8; 32] =
            secret_vec
                .as_slice()
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: format!(
                        "secret must be exactly 32 bytes (the Ed25519 seed), got {}",
                        secret_vec.len()
                    ),
                })?;
        let key_vec = slice_to_vec(device_key, device_key_len);
        let key: [u8; 32] =
            key_vec
                .as_slice()
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: format!("device_key must be exactly 32 bytes, got {}", key_vec.len()),
                })?;
        let identity = fauna_core::identity::ActorKeypair::from_secret(seed);
        let grant = fauna_client_sync::build_principal_grant(&identity, &key).map_err(|e| {
            crate::FfiError::General {
                msg: format!("sign device grant: {e}"),
            }
        })?;
        fauna_core::encoding::canonical_encode(&grant).map_err(|e| crate::FfiError::General {
            msg: format!("encode device grant: {e}"),
        })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Unwrap a subscription tier's period key out of a stored broadcast `KeyBlob`,
/// as the subscriber whose seed is `secret` — and report the blob's author.
///
/// **Why a test helper needs this at all.** The period key is the one thing that
/// makes "what you publish afterwards cannot be read by whoever held the old
/// key" observable: after a succession the aftermath's tier leg re-keys every
/// tier and republishes the covering blob, and the ONLY way to see that the KEY
/// really changed — rather than that the blob merely changed — is to unwrap it
/// the way a subscriber does. Re-implementing the unwrap in Python would witness
/// nothing: the class of bug this catches is precisely a client and a nest
/// disagreeing about the envelope, so the test has to ride the same
/// `fauna_core::subscription::crypto` seam every app rides (the rule
/// `fauna_atproto_delegation_cert_build` states for the mint side).
///
/// The blob's **author** comes back with the key because it is the era stamp the
/// rotation leg itself derives from: a blob published before the ceremony still
/// names the predecessor even after the succession re-pointed the row, so a test
/// can assert the flip directly instead of inferring it.
///
/// `secret` / `secret_len` — the subscriber's 32-byte Ed25519 seed.
/// `blob_data` / `blob_data_len` — `fauna.subscriptions.key_blob.get`'s
/// `blob_data` verbatim (the dag-cbor `EmbedAsBytes` wire shape).
/// `out` — receives 64 bytes: the 32-byte period key followed by the blob's
/// 32-byte author actor id. Caller frees via `fauna_ffi_free_buffer`.
///
/// Fails (non-zero, message via `fauna_ffi_last_error`) when the blob does not
/// decode or carries no entry for this subscriber — both are real findings for a
/// caller asserting a retained subscriber kept access, so neither degrades to an
/// empty buffer.
///
/// # Safety
///
/// `secret` and `blob_data` must each point to their stated number of readable
/// bytes; `out` must be a valid, aligned, writable pointer to an `FfiBuffer`.
/// All pointers must remain valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_keyblob_open(
    secret: *const u8,
    secret_len: u32,
    blob_data: *const u8,
    blob_data_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer/len pair meets `slice_to_vec`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let seed: [u8; 32] =
            secret_vec
                .as_slice()
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: format!(
                        "secret must be exactly 32 bytes (the Ed25519 seed), got {}",
                        secret_vec.len()
                    ),
                })?;
        let keypair = fauna_core::identity::ActorKeypair::from_secret(seed);
        let wire_vec = slice_to_vec(blob_data, blob_data_len);
        let wire: fauna_core::encoding::EmbedAsBytes =
            fauna_core::encoding::canonical_decode(&wire_vec).map_err(|e| {
                crate::FfiError::General {
                    msg: format!("decode key blob wire: {e}"),
                }
            })?;
        let blob: fauna_core::subscription::types::KeyBlob =
            fauna_core::encoding::decode_signed_bytes(&wire.bytes).map_err(|e| {
                crate::FfiError::General {
                    msg: format!("decode key blob: {e}"),
                }
            })?;
        let me = keypair.actor_id();
        let entry = blob
            .entries
            .iter()
            .find(|e| e.subscriber == me)
            .ok_or_else(|| crate::FfiError::General {
                msg: format!(
                    "the key blob wraps no entry for this subscriber ({} entries for tier {:?})",
                    blob.entries.len(),
                    blob.tier
                ),
            })?;
        let period_key = fauna_core::subscription::crypto::decrypt_key_blob_entry_for(
            &keypair, entry,
        )
        .map_err(|e| crate::FfiError::General {
            msg: format!("unwrap period key: {e}"),
        })?;
        let mut packed = period_key.to_vec();
        packed.extend_from_slice(&blob.author.0);
        Ok::<_, crate::FfiError>(packed)
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Build the birth `KeyBlob` envelope `fauna.subscriptions.tiers.create`
/// requires — an empty-roster blob for `tier` under `period_key`, self-signed
/// by the author whose 32-byte Ed25519 seed is `secret` — as the canonical
/// dag-cbor `EncryptedKeyBlobUpload` the request's `encrypted_upload` field
/// carries. Rides the same `mint_self_delegated_upload` every app's create
/// rides, so the e2e harness can never mint an envelope the nest would refuse
/// while an app's is accepted (or the reverse).
///
/// `tier` — NUL-terminated UTF-8. `period_key` — 32 bytes. `out` receives the
/// encoded upload; caller frees via `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes and `period_key` to 32;
/// `tier` must be a valid NUL-terminated C string; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`. All pointers must remain valid
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_subscriptions_build_birth_upload(
    secret: *const u8,
    secret_len: u32,
    tier: *const c_char,
    period_key: *const u8,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees
    // `secret`/`secret_len`, `period_key` and `tier` meet
    // `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let kp = crate::keypair_from_bytes(&secret_vec)?;
        let tier_s = cstr_to_string(tier)?;
        let pk: [u8; 32] =
            slice_to_vec(period_key, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "period_key must be 32 bytes".into(),
                })?;
        let upload = fauna_client_subscriptions::orchestration::mint_self_delegated_upload(
            &kp,
            &tier_s,
            fauna_core::data::Timestamp::now().0,
            &[],
            &pk,
        )
        .map_err(|e| crate::FfiError::General { msg: e.to_string() })?;
        fauna_core::encoding::canonical_encode(&upload).map_err(|e| crate::FfiError::General {
            msg: format!("encode birth upload: {e}"),
        })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Build the `EncryptedKeyBlobUpload` an author's approve carries — a
/// `KeyBlob` for `tier` under `period_key` covering exactly the
/// `subscriber_count` 32-byte actor ids packed back to back at `subscribers`,
/// at `rotated_at` (microseconds; must exceed every blob the tier already
/// published). The roster-bearing sibling of
/// [`fauna_subscriptions_build_birth_upload`], riding the same
/// `mint_self_delegated_upload`, so an e2e witness that keeps its tier's period
/// key can complete a grant through the real `requests.approve` door exactly as
/// an app's drain pump does — the nest itself only ever enqueues a grant
/// (`monetization.md` § Pillar 1 → the headless-author corollary).
///
/// A roster entry is hybrid (X-Wing) exactly when its subscriber published an
/// ML-KEM encapsulation key, as an app's drain pump wraps: `mlkem_eks` holds
/// `1184 * subscriber_count` bytes, one slot per entry in roster order, and
/// `mlkem_ek_present` one byte per entry (non-zero = the slot is that
/// subscriber's key; zero = classical, the slot ignored). Both null =
/// every entry classical. `out` receives the canonical dag-cbor upload;
/// caller frees via `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes, `period_key` to 32 and
/// `subscribers` to `32 * subscriber_count`; when non-null, `mlkem_eks` to
/// `1184 * subscriber_count` and `mlkem_ek_present` to `subscriber_count`;
/// `tier` must be a valid NUL-terminated C string; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`. All pointers must remain valid
/// for the duration of the call.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn fauna_subscriptions_build_roster_upload(
    secret: *const u8,
    secret_len: u32,
    tier: *const c_char,
    period_key: *const u8,
    rotated_at: u64,
    subscribers: *const u8,
    subscriber_count: u32,
    mlkem_eks: *const u8,
    mlkem_ek_present: *const u8,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer/length pair meets `slice_to_vec`/`cstr_to_string`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let secret_vec = slice_to_vec(secret, secret_len);
        let kp = crate::keypair_from_bytes(&secret_vec)?;
        let tier_s = cstr_to_string(tier)?;
        let pk: [u8; 32] =
            slice_to_vec(period_key, 32)
                .try_into()
                .map_err(|_| crate::FfiError::General {
                    msg: "period_key must be 32 bytes".into(),
                })?;
        let packed = slice_to_vec(subscribers, subscriber_count.saturating_mul(32));
        const EK_LEN: u32 = 1184;
        let (eks, present) = if mlkem_eks.is_null() || mlkem_ek_present.is_null() {
            (Vec::new(), vec![0u8; subscriber_count as usize])
        } else {
            (
                slice_to_vec(mlkem_eks, subscriber_count.saturating_mul(EK_LEN)),
                slice_to_vec(mlkem_ek_present, subscriber_count),
            )
        };
        let roster: Vec<(fauna_core::identity::ActorId, Option<Vec<u8>>)> = packed
            .as_chunks::<32>()
            .0
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut id = [0u8; 32];
                id.copy_from_slice(c);
                let ek = (present[i] != 0).then(|| {
                    let at = i * EK_LEN as usize;
                    eks[at..at + EK_LEN as usize].to_vec()
                });
                (fauna_core::identity::ActorId(id), ek)
            })
            .collect();
        let upload = fauna_client_subscriptions::orchestration::mint_self_delegated_upload(
            &kp, &tier_s, rotated_at, &roster, &pk,
        )
        .map_err(|e| crate::FfiError::General { msg: e.to_string() })?;
        fauna_core::encoding::canonical_encode(&upload).map_err(|e| crate::FfiError::General {
            msg: format!("encode roster upload: {e}"),
        })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Build the `fauna.subscriptions.encaps_key.publish` request an account's
/// app sends at session start — its identity-seed-derived ML-KEM-768
/// encapsulation key (`post-quantum.md` § surface B) — through the one shared
/// builder `fauna_client_subscriptions::encaps_key_publish_request`, so an
/// e2e witness publishes exactly what an app does. `out` receives the
/// canonical dag-cbor request; caller frees via `fauna_ffi_free_buffer`.
///
/// # Safety
///
/// `secret` must point to `secret_len` readable bytes; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_subscriptions_build_encaps_key_publish(
    secret: *const u8,
    secret_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, `secret`/`secret_len` meet
    // `slice_to_vec`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let kp = crate::keypair_from_bytes(&slice_to_vec(secret, secret_len))?;
        let req = fauna_client_subscriptions::encaps_key_publish_request(&kp);
        fauna_core::encoding::canonical_encode(&req).map_err(|e| crate::FfiError::General {
            msg: format!("encode encaps key publish: {e}"),
        })
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

/// Build the `fauna.tls.publish_cert` request an admin's app sends after a
/// DNS-01 order finishes: the issued cert sealed to the nest it is for and
/// signed by the admin, through the one shared producer
/// `fauna_client_dns::seal_issued_cert_for_nest` the admin-dns machine's
/// delivery calls — so a witness of the relayed install publishes exactly what
/// an app does. `out` receives the canonical dag-cbor request; caller frees via
/// `fauna_ffi_free_buffer`.
///
/// `cert_chain_pem` / `privkey_pem` / `domain` — NUL-terminated UTF-8.
/// `target_nest_id` — the installing nest's 32-byte Ed25519 node id.
/// `actor_secret` — the signer's 32-byte Ed25519 seed.
///
/// # Safety
///
/// The three strings must be valid NUL-terminated C strings; `target_nest_id`
/// and `actor_secret` must point to 32 readable bytes; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`. All pointers must remain valid
/// for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_tls_build_publish_cert(
    cert_chain_pem: *const c_char,
    privkey_pem: *const c_char,
    domain: *const c_char,
    target_nest_id: *const u8,
    actor_secret: *const u8,
    out: *mut FfiBuffer,
) -> i32 {
    // SAFETY: per the `# Safety` section above, the caller guarantees every
    // pointer meets `cstr_to_string`/`read_32`'s preconditions.
    let bytes = cabi_try!(unsafe {
        let general = |msg: String| crate::FfiError::General { msg };
        let issued = fauna_client_dns::Dns01Issued {
            cert_chain_pem: cstr_to_string(cert_chain_pem)?,
            privkey_pem: cstr_to_string(privkey_pem)?,
            account_credentials: Vec::new(),
        };
        let domain = cstr_to_string(domain)?;
        let nest_id = read_32(target_nest_id, "target_nest_id")?;
        let actor_sk =
            ed25519_dalek::SigningKey::from_bytes(&read_32(actor_secret, "actor_secret")?);
        let (ciphertext, actor_sig) =
            fauna_client_dns::seal_issued_cert_for_nest(&issued, &domain, &nest_id, &actor_sk)
                .map_err(|e| general(e.to_string()))?;
        let req = fauna_protocol::tls::PublishCertRequest {
            ciphertext: fauna_protocol::ByteBuf::from(ciphertext),
            actor_sig: fauna_protocol::ByteBuf::from(actor_sig),
            extra: Default::default(),
        };
        fauna_core::encoding::canonical_encode(&req)
            .map_err(|e| general(format!("encode publish_cert request: {e}")))
    });
    // SAFETY: per the `# Safety` section above, `out` is a valid, aligned,
    // writable pointer to an `FfiBuffer` for the duration of this call.
    unsafe {
        *out = FfiBuffer::from_vec(bytes);
    }
    0
}

// The harness's networked seed exports — test surface, compiled out of every
// release artifact (e2e convention 15); see the module doc.
#[cfg(feature = "e2e-harness")]
mod harness;
