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

/// Build the canonical-CBOR **consent-time folder deposit grant to a
/// third-party principal** — one keyless `deposit` tuple over `folder_id`,
/// held by `holder_x25519` (`file-sync.md` § Third-party deposit ingress).
/// The Python e2e twin of the approving app's mint: it runs the production
/// `fauna_client_capabilities::mint_folder_deposit_grant`, owned by the actor
/// `identity_seed` derives.
///
/// # Safety
///
/// `identity_seed` (32), `grant_id` (16) and `holder_x25519` (32) must point
/// to the stated byte counts; `out` must be a valid, aligned, writable
/// `FfiBuffer` pointer. All pointers must remain valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_capability_build_folder_deposit_grant(
    identity_seed: *const u8,
    grant_id: *const u8,
    holder_x25519: *const u8,
    epoch_start: u64,
    epoch_end: u64,
    folder_id: i64,
    out: *mut FfiBuffer,
) -> i32 {
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
        let blob = fauna_client_capabilities::mint_folder_deposit_grant(
            &keypair.actor_id().0,
            &grant_a,
            &holder,
            GrantWindow(epoch_start, epoch_end),
            &[folder_id],
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

// The harness's networked seed exports — test surface, compiled out of every
// release artifact (e2e convention 15); see the module doc.
#[cfg(feature = "e2e-harness")]
mod harness;
