//! UniFFI façade for the shared spam-preferences contract — both the
//! *presentation* helper (`fauna_protocol::spam::spam_threshold_band`)
//! and the *transport* surface ([`FfiSpamClient`] over
//! `fauna_client_spam::SpamClient`).
//!
//! The presentation helper gives Apple / Windows / Android the same
//! threshold-band buckets the Rust-native Linux app reads directly, so no client re-derives the goal-doc's mapping per
//! platform (priority #2/#4; the spam analog of
//! `email_client::encode_email_filter_rule`). See `docs/goal/ui/settings.md`
//! § Spam threshold slider labels.
//!
//! [`FfiSpamClient`] is the native-client twin of the wire CRUD the Linux
//! app calls `fauna_client_spam::SpamClient` for directly — it lets Apple /
//! Windows / Android read & write `fauna.spam.{get,set}_preferences` over
//! WS-RPC instead of the legacy `GET|PUT /api/v1/spam/preferences` HTTP twin
//! (the client seam). The wire carries the
//! thresholds as per-mille `u16` (dag-cbor forbids floats); the UI presents
//! them as a 0.0–1.0 slider, so the client divides/multiplies by 1000 (as
//! Linux's `privacy.rs` does).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_spam::SpamClient;
use fauna_client_spam::spam::{
    SpamGetPreferencesRequest, SpamPreferences, SpamSetPreferencesRequest,
};
use fauna_protocol::spam;

use crate::{FfiError, stringify};

/// UniFFI face of [`fauna_protocol::spam::spam_threshold_band`] — map a
/// per-mille spam threshold (`0–1000`) to its label-band i18n key
/// (`aggressive`/`moderate`/`permissive`). The client localizes the key via the
/// shared `spam:` i18n strings, giving every app the identical band buckets.
#[uniffi::export]
pub fn spam_threshold_band(threshold_per_mille: u16) -> String {
    spam::spam_threshold_band(threshold_per_mille)
        .key()
        .to_string()
}

// ── transport: FfiSpamClient ───────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::spam::SpamPreferences`] — the calling
/// actor's spam-classifier preferences. The two thresholds are **per-mille**
/// `u16` (`0–1000`); a client presents them as a 0.0–1.0 slider by dividing by
/// 1000 (the wire forbids floats). The protocol type's forward-compat `extra`
/// overflow map is intentionally dropped — clients only consume the named fields.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSpamPreferences {
    pub spam_threshold: u16,
    pub phishing_threshold: u16,
}

impl From<SpamPreferences> for FfiSpamPreferences {
    fn from(p: SpamPreferences) -> Self {
        FfiSpamPreferences {
            spam_threshold: p.spam_threshold,
            phishing_threshold: p.phishing_threshold,
        }
    }
}

/// UniFFI handle for the `fauna.spam.*` preferences kinds. Construct via
/// [`crate::nest_client::FfiNestClient::spam`]; methods are exposed to Swift as
/// `async throws` and Kotlin as `suspend fun`. Thin wrapper over the shared
/// `fauna_client_spam::SpamClient` (priority #2 — the kind-composition logic is
/// written once there and shared across native + wasm).
#[derive(uniffi::Object)]
pub struct FfiSpamClient {
    nest: Arc<NestClient>,
}

impl FfiSpamClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> SpamClient<Arc<NestClient>> {
        SpamClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiSpamClient {
    /// `fauna.spam.get_preferences` — read the calling actor's spam-classifier
    /// preferences (the subject is implicit — the connection knows its caller).
    pub async fn get_preferences(&self) -> Result<FfiSpamPreferences, FfiError> {
        let prefs = self
            .client()
            .get_preferences(SpamGetPreferencesRequest {
                extra: Default::default(),
            })
            .await
            .map_err(stringify)?;
        Ok(prefs.into())
    }

    /// `fauna.spam.set_preferences` — partial update; each `None` field is left
    /// unchanged (mirrors the HTTP twin's per-field semantics). The nest clamps
    /// the thresholds to `[0, 1000]` per-mille. The reply echoes the
    /// resulting full preferences.
    pub async fn set_preferences(
        &self,
        spam_threshold: Option<u16>,
        phishing_threshold: Option<u16>,
    ) -> Result<FfiSpamPreferences, FfiError> {
        let prefs = self
            .client()
            .set_preferences(SpamSetPreferencesRequest {
                spam_threshold,
                phishing_threshold,
                extra: Default::default(),
            })
            .await
            .map_err(stringify)?;
        Ok(prefs.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_export_returns_i18n_keys() {
        assert_eq!(spam_threshold_band(0), "aggressive");
        assert_eq!(spam_threshold_band(500), "moderate");
        assert_eq!(spam_threshold_band(900), "permissive");
    }

    #[test]
    fn preferences_mirror_maps_all_named_fields() {
        // The `extra` overflow map has no FFI counterpart (the mirror has no
        // such field), so the conversion drops it by construction; here we pin
        // that every *named* field crosses unchanged (per-mille u16 thresholds).
        let proto = SpamPreferences {
            spam_threshold: 800,
            phishing_threshold: 600,
            extra: Default::default(),
        };
        let ffi: FfiSpamPreferences = proto.into();
        assert_eq!(
            ffi,
            FfiSpamPreferences {
                spam_threshold: 800,
                phishing_threshold: 600,
            }
        );
    }
}
