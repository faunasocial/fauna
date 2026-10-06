//! Typed-call wrapper for the spam-classifier preferences WS-RPC kinds —
//! `fauna.spam.{get_preferences,set_preferences}`. The user's local
//! spam/phishing thresholds, edited
//! from the Settings → Privacy page.
//!
//! Faithful transport migration of `GET|PUT /api/v1/spam/preferences`
//! (tracked internally — client seam + linux migration; the HTTP twins
//! stay until every app is off them).
//!
//! Pattern: same shape as the sibling per-feature client wrappers
//! (`fauna-client-bridges`, `-conversations`, `-subscriptions`,
//! `-search`) — a thin `pub struct SpamClient { nest: R }`, one async
//! method per kind, no state machine. Generic over the WS-RPC transport
//! (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm
//! SPA passes its `WsRpcClient`. The kind-composition logic is written once
//! here and shared across native + wasm (priority #2).

use fauna_protocol::RpcRequester;
use fauna_protocol::spam::{SpamGetPreferencesRequest, SpamPreferences, SpamSetPreferencesRequest};

pub use fauna_protocol::spam;

/// Typed `fauna.spam.*` call surface. Errors propagate as the transport's
/// `R::Error` (native `NestClientError`, wasm rpc-wasm error); the namespaced
/// `RpcError`s the handlers emit (`fauna.spam.invalid_params`,
/// `fauna.spam.permission_denied`) surface through that error channel.
pub struct SpamClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> SpamClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.spam.get_preferences` — read the calling actor's spam-classifier
    /// preferences (the subject is implicit — the connection knows its caller).
    /// Replay-safe pure read (`forbid_replay=false`, 5 s deadline per
    /// `register_spam_kinds`).
    pub async fn get_preferences(
        &self,
        req: SpamGetPreferencesRequest,
    ) -> Result<SpamPreferences, R::Error> {
        self.nest.request("fauna.spam.get_preferences", req).await
    }

    /// `fauna.spam.set_preferences` — partial update; only the `Some(_)`
    /// fields change (mirrors the HTTP twin's per-field semantics). The nest
    /// clamps the thresholds to `[0, 1000]` per-mille (wire is integer — the
    /// dag-cbor wire forbids floats).
    /// The reply echoes the resulting full `SpamPreferences`. Idempotent
    /// (`forbid_replay=false`): setting the same prefs twice = same state.
    pub async fn set_preferences(
        &self,
        req: SpamSetPreferencesRequest,
    ) -> Result<SpamPreferences, R::Error> {
        self.nest.request("fauna.spam.set_preferences", req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = SpamClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The construction-only `MockRequester` above can't catch a wrong kind
    // string or a request that no longer serializes to the shape the nest
    // handler decodes. These tests pin both: each `SpamClient` method must send
    // its exact `fauna.spam.*` kind and a payload that round-trips back to the
    // typed request. No nest-side conformance test routes through this adapter's
    // literal kind strings, so without these an adapter-method kind rename would
    // break silently. The pattern mirrors `fauna-client-events`'s
    // `RecordingRequester` (transport-free, so it runs on every target including
    // wasm); real end-to-end round-trip conformance lives in the nest-side spam
    // suite (`bins/fauna-nest/tests/conformance_spam.rs`).

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        // Answer with a reply the requested `Reply` type decodes — both
        // spam kinds reply with the full `SpamPreferences`.
        match kind {
            "fauna.spam.get_preferences" | "fauna.spam.set_preferences" => {
                fauna_protocol::encode_canonical(&SpamPreferences {
                    spam_threshold: 800,
                    phishing_threshold: 600,
                    extra: Default::default(),
                })
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn get_preferences_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SpamClient::new(rec.clone());
        block_on(client.get_preferences(SpamGetPreferencesRequest {
            extra: Default::default(),
        }))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.spam.get_preferences");
        let req: SpamGetPreferencesRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(req.extra.is_empty());
    }

    #[test]
    fn set_preferences_composes_kind_and_payload() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = SpamClient::new(rec.clone());
        block_on(client.set_preferences(SpamSetPreferencesRequest {
            spam_threshold: Some(420),
            phishing_threshold: None,
            extra: Default::default(),
        }))
        .expect("infallible mock");

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.spam.set_preferences");
        let req: SpamSetPreferencesRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.spam_threshold, Some(420));
        assert_eq!(req.phishing_threshold, None);
    }
}
