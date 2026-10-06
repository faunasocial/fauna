//! The arm for targets with **no OS-native credential store this crate drives**
//! — in practice the phone builds (iOS, watchOS, tvOS, Android) plus anything
//! else that is neither macOS, Windows, nor freedesktop Linux.
//!
//! # Why this arm exists at all
//!
//! Nothing on a phone *should* resolve credentials through this crate: the
//! apple apps hold their secrets in the Swift-side Keychain and android in its
//! own keystore. The crate nevertheless has to **compile** for those targets,
//! because `fauna-ffi` exports the desktop sync-agent provisioning face on
//! every apple slice "for flat-binding consistency ... but only macOS drives
//! it" (`fauna-ffi/src/lib.rs`, the `sync_agent_provisioning` module comment) —
//! one shared FaunaKit Swift target compiles against one binding surface for
//! macOS and iOS alike, which is priority #1 (minimize per-app divergence)
//! bought at the price of the module being *present* on slices that never call
//! it. `fauna-client-sync`'s `#[cfg(any(unix, windows))]` agent module is what
//! carries this crate there, and phones are `unix`.
//!
//! # Why it is an allow-list, not an exclusion list
//!
//! Until 2026-08-12 the freedesktop arm was gated
//! `cfg(not(any(target_os = "macos", target_os = "windows")))` — an exclusion
//! list that silently reads as *"every target that isn't macOS or Windows
//! speaks D-Bus"*. That is false for every phone target, so the moment
//! `fauna-client-sync` grew a `fauna-credential-store` dependency the iOS,
//! watchOS and Android slices began pulling `secret-service` → `zbus`, and the
//! full `just apple-ffi` stopped compiling (`zbus` has no Apple-phone arm for
//! `get_unix_peer_creds_blocking`). The arms are now positive and total —
//! macOS / Windows / Linux each named, everything else landing here — so a
//! target this crate has never heard of can only ever fall back to the inert
//! arm below. It can no longer be mistaken for a Linux desktop.
//!
//! # Behavior
//!
//! Inert, and **loud rather than silent**. `keyring_probe` reports "no usable
//! keyring", which is what steers [`crate::CredentialStore::new_with_headless_fallback`]
//! onto the sealed passphrase backend — a real, working store — rather than
//! onto a keyring that would swallow writes. The mutating ops warn if they are
//! ever reached, because reaching them means a *caller* wandered onto a path
//! this platform was never supposed to drive; that is a bug to see, not to
//! absorb quietly.

/// Always `None` — this target has no keyring arm to read.
pub fn keyring_get(_app: &str, _account: &str) -> Option<String> {
    None
}

/// No-op + warn: a credential write reaching here would otherwise vanish
/// without trace. Matches the infallible `SecretStore::set` contract the other
/// arms present, so callers need no per-platform branch.
pub fn keyring_set(_app: &str, account: &str, _value: &str) {
    tracing::warn!(
        account,
        "credential store: no OS keyring on this target; write dropped \
         (this platform's app owns its own secure store — see no_keyring.rs)"
    );
}

/// No-op + warn, for the same reason as [`keyring_set`].
pub fn keyring_delete(_app: &str, account: &str) {
    tracing::warn!(
        account,
        "credential store: no OS keyring on this target; delete ignored \
         (this platform's app owns its own secure store — see no_keyring.rs)"
    );
}

/// Success: a namespace that was never writable here is already empty, which is
/// exactly the "delete-nothing-found is success" contract the macOS, Windows and
/// freedesktop arms all present.
pub fn keyring_delete_namespace(_app: &str) -> Result<(), anyhow::Error> {
    Ok(())
}

/// Always `false` — "no usable OS keyring here", steering the headless-fallback
/// resolution onto the sealed backend.
pub fn keyring_probe() -> bool {
    false
}
