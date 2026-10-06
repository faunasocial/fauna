use crate::{FfiError, keypair_from_bytes};

/// Generate a new Ed25519 keypair. Returns 32-byte secret key.
#[uniffi::export]
pub fn generate_keypair() -> Vec<u8> {
    let kp = fauna_client_core::identity::generate_keypair();
    kp.signing_key().to_bytes().to_vec()
}

/// Derive the public ActorId from a 32-byte secret key.
/// Returns 32-byte public key.
#[uniffi::export]
pub fn actor_id_from_secret(secret: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    Ok(fauna_client_core::identity::actor_id(&kp).to_vec())
}

/// Encode a 64-char hex secret key into a `fauna://identity?secret=…[&handle=…]` URI.
/// When `handle` is present (and non-empty), the QR carries the `(identity, handle)`
/// payload so a scanned import pre-fills the handle step (onboarding.md §1.identity_import).
#[uniffi::export]
pub fn identity_qr_encode(secret_hex: String, handle: Option<String>) -> String {
    fauna_core::identity_qr::IdentityQr::to_uri(&secret_hex, handle.as_deref())
}

/// Decode a `fauna://identity?secret=…` URI, returning the 64-char hex secret.
#[uniffi::export]
pub fn identity_qr_decode(uri: String) -> Result<String, FfiError> {
    fauna_core::identity_qr::IdentityQr::from_uri(&uri).map_err(|e| FfiError::General { msg: e })
}

/// UniFFI face of [`fauna_core::identity_qr::parse_import_input`] — parse a pasted/scanned
/// identity-import field (bare 64-hex secret, `fauna://identity?secret=&handle=` query
/// form, or the iOS colon form) into a flat `[secret, handle]` list (`handle` = `""` when
/// absent). An **empty list** signals a parse failure. iOS/android share this with web
/// instead of hand-rolling the input grammar (priority #2/#4).
#[uniffi::export]
pub fn parse_identity_import(input: String) -> Vec<String> {
    fauna_core::identity_qr::parse_import_input(&input)
        .map(|i| i.into_parts())
        .unwrap_or_default()
}

/// UniFFI face of [`fauna_core::format::short_id`] — the canonical short display
/// form for a long hex id (first 12 chars + `…`). Native apps share this with
/// web instead of re-deriving `.prefix(12)+"..."` / `.take(12)+"..."` /
/// `&hex[..12]` (priority #1/#4). See `docs/goal/behavior/value-formatting.md`.
#[uniffi::export]
pub fn short_id(hex: String) -> String {
    fauna_core::format::short_id(&hex)
}

/// UniFFI face of [`fauna_core::format::hex_short`] — the short hex display of an
/// id given as raw bytes (first 4 bytes → 8 lowercase, zero-padded hex chars).
/// Android/Windows share this for the non-local backup-restore source label
/// (`source_member_id` → destination short hex) instead of re-deriving the
/// per-byte hex (priority #1/#4); Linux calls `fauna_core::format::hex_short`
/// directly (Rust dep), web via a wasm `hexShort` export. Distinct from
/// [`short_id`] (a 64-hex *string* → 12 chars + `…`). See
/// `docs/goal/ui/backups.md` § Where logic lives.
#[uniffi::export]
pub fn hex_short(bytes: Vec<u8>) -> String {
    fauna_core::format::hex_short(&bytes)
}

/// UniFFI face of [`fauna_core::format::short_nest_id`] — the head…tail elision
/// of a long hex `nest_actor_id` for a box-recovery row label (first 8 chars +
/// `…` + last 8 chars). Windows shares this instead of re-deriving its own
/// `ShortNestId` (its own comment already flagged the gap); Linux calls
/// `fauna_core::format::short_nest_id` directly (Rust dep), web via a wasm
/// `shortNestId` export. Distinct from [`short_id`] (a 12-char *prefix*, no
/// tail). See `docs/goal/behavior/value-formatting.md` § Short nest id.
#[uniffi::export]
pub fn short_nest_id(id: String) -> String {
    fauna_core::format::short_nest_id(&id)
}

/// UniFFI face of [`fauna_core::format::account_display_label`] — the
/// account-switcher row title: the cached handle when present and non-empty,
/// else the canonical [`short_id`] of the actor id. Native apps share this
/// instead of re-deriving the handle-or-fallback branch (priority #1/#4); web
/// via a wasm `accountDisplayLabel` export.
#[uniffi::export]
pub fn account_display_label(handle: Option<String>, actor_id: String) -> String {
    fauna_core::format::account_display_label(handle.as_deref(), &actor_id)
}

/// UniFFI face of [`fauna_core::format::profile_nav_target`] — resolve a
/// test-agent profile-navigation actor id: `None` when it names the viewer
/// (open SELF), `Some(trimmed id)` otherwise (open that OTHER actor). Native
/// apps share this instead of re-deriving the trim + ASCII-case-insensitive
/// self-compare (priority #1/#2) — linux/tui/android each hand-rolled their
/// own copy before this lift. See `docs/goal/ui/profile.md` § Layout & flow →
/// Another's profile.
#[uniffi::export]
pub fn profile_nav_target(entry_actor_id: String, self_actor_id: Option<String>) -> Option<String> {
    fauna_core::format::profile_nav_target(&entry_actor_id, self_actor_id.as_deref())
}
