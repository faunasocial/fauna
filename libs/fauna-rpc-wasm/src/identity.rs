//! `secretHex` → [`ActorKeypair`], `JsValue`-erroring — the ceremony every
//! `fauna-wasm-folders`/`fauna-wasm-media` `set*` wiring call repeats before
//! handing the keypair to a `NestFolderKeyResolver` or
//! similar RPC-backed helper. Nine call sites hand-copied this exact
//! two-line shape (their own bodies, not just their names, were identical).

use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use wasm_bindgen::JsValue;

/// Parse the caller's 32-byte actor secret, hex-encoded, exactly as every
/// `set*Source`/`set*Custody` wasm export receives it.
pub fn keypair_from_secret_hex(secret_hex: &str) -> Result<ActorKeypair, JsValue> {
    ActorKeypair::from_secret_hex(secret_hex)
        .map_err(|e| JsValue::from_str(&format!("invalid secretHex: {e}")))
}

/// [`keypair_from_secret_hex`] + `make(client, keypair)` + `Arc::new` — the
/// three-step shape every `set*Source(secretHex)` wasm setter that hands a
/// freshly-derived keypair to a client-backed source repeats
/// (`fauna-wasm-folders`'s `setForeignSetsSource`/`setFollowedFoldersSource`,
/// `fauna-wasm-media`'s `setFollowedMediaSource`). Deliberately generic over
/// both `client`'s type and the constructed `T` — this crate need not name
/// `fauna-devices-machine`'s source types to share the ceremony around them.
pub fn arc_from_secret_hex<R, T>(
    client: R,
    secret_hex: &str,
    make: impl FnOnce(R, ActorKeypair) -> T,
) -> Result<Arc<T>, JsValue> {
    let keypair = keypair_from_secret_hex(secret_hex)?;
    Ok(Arc::new(make(client, keypair)))
}
