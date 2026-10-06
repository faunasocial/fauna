//! Web nest-identity TOFU pin glue (`docs/goal/architecture/security.md` § Transport trust — the web-exempt
//! note): the distinctive rejection the launch flow surfaces as the "nest
//! identity changed" warning, and the explicit "trust this nest" re-pin
//! export. The pin/compare + possession-verify logic AND the
//! localStorage-backed store both live in `fauna_client_core::nest_trust`
//! ([`LocalStoragePinStore`] — shared with the launch machine's wasm
//! silent-challenge connector, one store over one localStorage key); only the
//! JS-facing error shaping and the `forgetNestIdentityPin` binding live here
//! (priority #2).

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use fauna_client_core::nest_trust::{NestIdentityPinStore, WebIdentityError};

pub use fauna_client_core::nest_trust::LocalStoragePinStore;

/// Build the distinctive rejection the SPA classifies into a
/// `NestIdentityChangedError` (→ the launch "nest identity changed" warning).
/// Carries the origin + fingerprints (`seen` is `null` for a withdrawn proof) so
/// the warning can surface them. `fork: true` marks rotation-chain fork
/// evidence (`box-recovery.md` § Client acceptance) — the warning surface must
/// offer NO re-trust for it; the additive JSON field is ignored by older SPA
/// bundles (which keep today's warning) and read by the fork-aware one.
pub(crate) fn nest_identity_error_js(origin: &str, e: &WebIdentityError, fork: bool) -> JsValue {
    let (pinned, seen) = match e {
        WebIdentityError::Changed { pinned, seen } => {
            (hex::encode(pinned), Some(hex::encode(seen)))
        }
        WebIdentityError::Withdrawn { pinned } => (hex::encode(pinned), None),
    };
    let payload =
        serde_json::json!({ "origin": origin, "pinned": pinned, "seen": seen, "fork": fork });
    JsValue::from_str(&format!("nest-identity-changed:{payload}"))
}

/// The **one** web pin verdict, shared by every channel that can produce it.
///
/// `seen` is the identity this connect possession-proved (`None` when the nest
/// served no usable `cert_binding` — a plaintext/dev nest, or a withdrawn
/// proof). Compares it against the pin, bridges a committed deployment-seed
/// rotation when the chain proves one (`box-recovery.md` § Client acceptance),
/// and otherwise returns the distinctive rejection the SPA routes to
/// `launch_identity_changed`.
///
/// **Why this is a function and not two copies.** Web reaches the verdict from
/// two independent channels — the launch/background silent challenge
/// (`fauna.auth.verify`) and the bearer re-mint (`fauna.auth.handshake`,
/// `security.md` § Post-auth surfacing channel 1) — which differ only in what
/// they possession-verify the binding *over* (challenge nonce ‖ client nonce vs
/// the handshake's client nonce alone). Everything downstream of `seen` is
/// identical, and a second copy is exactly how one channel silently drifts into
/// re-pinning where the other blocks.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn check_pin_and_maybe_repin(
    client: &fauna_rpc_wasm::AnonymousWsRpcClient,
    origin: &str,
    seen: Option<[u8; 32]>,
) -> Result<(), JsValue> {
    use fauna_client_core::nest_trust::{
        RotationRepin, check_web_nest_identity, try_rotation_repin,
    };

    let Err(e) = check_web_nest_identity(seen, origin, &LocalStoragePinStore) else {
        return Ok(());
    };
    // A *changed* identity may be a committed deployment-seed rotation: fetch
    // the box's rotation chain over this same connection and re-pin silently
    // when it bridges pinned → the possession-proven identity. Withdrawn has no
    // live proof to bridge to.
    let fork = match &e {
        WebIdentityError::Changed { pinned, seen } => {
            match try_rotation_repin(client, origin, &LocalStoragePinStore, *pinned, *seen).await {
                RotationRepin::Repinned { .. } => return Ok(()),
                RotationRepin::Fork => true,
                RotationRepin::NoBridge { .. } => false,
            }
        }
        WebIdentityError::Withdrawn { .. } => false,
    };
    Err(nest_identity_error_js(origin, &e, fork))
}

/// Explicit user-approved recovery: forget the pinned identity for `origin` so
/// the next connect re-establishes trust on first use — the warning UI's "trust
/// this nest" button (the browser analogue of `ssh-keygen -R host`). Never
/// called automatically; a pin only changes via a user action or a clean
/// first-connect.
#[wasm_bindgen(js_name = forgetNestIdentityPin)]
pub fn forget_nest_identity_pin(origin: &str) {
    LocalStoragePinStore.remove(origin);
}
