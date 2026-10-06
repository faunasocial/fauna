//! Typed-call wrapper + render derivations for the **controversial-class
//! feature plane's transparency read** (`fauna.features.status` —
//! `docs/goal/architecture/dynamic-features.md` § Transparency & auditability,
//! § Evaluation points item 2).
//!
//! Boundary 4 of § What this is NOT is why this crate exists: *"No silent
//! gates. Every active restriction is visible to the person it binds — which
//! feature, what limit, which tier set it."* The nest evaluates and enforces
//! the plane; this is the **courtesy layer** — the shared half of what all 7
//! apps render, so the answer to "why can't I do this" cannot differ per app
//! (priority #1) and is written once (priority #2).
//!
//! The **authoring** half — the one editor an admin writes the nest-wide
//! document with and a person limits themselves with — is [`editor`]
//! (§ Authoring surfaces).
//!
//! Pattern: the same shape as [`fauna_client_family::FamilyClient`] — a thin
//! `pub struct FeaturesClient<R: RpcRequester>`, one async method per kind, no
//! state machine. Native call sites pass `Arc<NestClient>`, the wasm SPA its
//! `WsRpcClient`. The derivations in [`view_model`] are pure and clock-free.
//!
//! # The one trap, and it is mutation-proven nest-side
//!
//! **Render the [`EffectivePolicy`] the nest sends; never recompose the meet
//! from authored documents.** The subset edge (`zaps` inherits a `payments`
//! deny) fires only when the caller passes the superset's authored documents,
//! and an empty policy slice legitimately means *"the superset allows"* — so
//! nothing inside `fauna_core::feature_gate` can force the correct call. Nest
//! side that is why exactly one resolver exists
//! (`fauna_nest::feature_gate::resolve_effective_policy`); client side the
//! same rule applies, and a client that recomposed would show `zaps` as
//! available under a `payments` deny the nest refuses — a silent gate wearing
//! the opposite costume. This crate therefore calls
//! `fauna_core::feature_gate::effective_policy` **nowhere**, which is pinned by
//! a source-level test rather than left to a comment
//! ([`view_model::tests::this_crate_never_recomposes_the_meet`]).
//!
//! # What is *not* here
//!
//! Enforcement. Nest-side gating is the floor and holds against a
//! non-conforming or version-skewed client (§ Evaluation points item 1); every
//! affordance decision below is courtesy on top of that floor, never a
//! substitute for it.

pub mod editor;
pub mod row;
#[cfg(feature = "test-fixtures")]
pub mod test_fixtures;
pub mod view_model;

use fauna_protocol::RpcRequester;
use fauna_protocol::discovery::{NestInfoReply, NestInfoRequest};
use fauna_protocol::features::{FeaturesStatusReply, FeaturesStatusRequest};

// Re-exported so a client reads the feature-plane shapes through this wrapper
// crate's typed surface without depending on `fauna-protocol` /
// `fauna-core::feature_gate` directly (mirrors `fauna-client-admin`).
pub use editor::{
    AuthoredRow, AuthoredSurface, AuthoringTier, EditorCell, EditorSlot, PolicyEditor,
    PolicyEditorView, SaveError, Saved, authored_row, authored_summary, authored_surface,
    editor_note_text, editor_slots, removed_text, saved_text, ward_authored_items,
    ward_policy_with, ward_surface,
};
pub use fauna_core::feature_gate::{
    Availability, BoundSource, EffectiveBounds, EffectivePolicy, GatedFeature, MagnitudeUnit,
    QuotaDimension, RuleTier, UsageCounters, Window, WindowCounts,
};
pub use fauna_protocol::features::{AuthoredPolicyItem, FeaturePolicyReadReply};
pub use fauna_protocol::features::{FeatureStatusItem, FeaturesStatusReply as StatusReply};
pub use row::{
    CellMagnitudes, FeatureRow, RowCell, cell_value_text, dimension_label, feature_row,
    feature_rows, gate_reason, gated_feature_keys, window_label,
};
pub use view_model::{
    Affordance, FeatureLimits, LimitCell, affordance, capability_token, counterparty_headroom,
    feature_label, feature_limits, remaining, restriction_text, tier_label,
};

/// Typed `fauna.features.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`).
pub struct FeaturesClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> FeaturesClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.features.status` — the caller's **own** effective feature policy
    /// plus what they have already spent, for every registry member.
    ///
    /// No parameters: the answer is about the bearer, and there is no read that
    /// names another account. The reply is always complete — a feature the
    /// caller is not limited on still appears, with its tier-1 bounds, because
    /// "unrestricted" is an answer and an absence could not be told apart from
    /// a nest that never heard of the feature.
    pub async fn status(&self) -> Result<FeaturesStatusReply, R::Error> {
        self.nest
            .request("fauna.features.status", FeaturesStatusRequest::default())
            .await
    }

    /// `fauna.nest.info` → the nest's advertised capability set.
    ///
    /// Not a feature-plane kind, but the second half of what a row needs: the
    /// `hidden` affordance is decided by a capability token's absence, and there
    /// is no other source for it.
    pub async fn node_capabilities(&self) -> Result<Vec<String>, R::Error> {
        let reply: NestInfoReply = self
            .nest
            .request("fauna.nest.info", NestInfoRequest::default())
            .await?;
        Ok(reply.capabilities)
    }

    /// **The call an app makes** — the whole feature-limits surface, ready to
    /// render: [`status`](Self::status) joined with
    /// [`node_capabilities`](Self::node_capabilities) and folded by
    /// [`feature_rows`].
    ///
    /// Both reads live behind one method deliberately. The capability set is
    /// what makes a row `hidden`, and that arm is load-bearing rather than
    /// theoretical: `fauna_core::feature_gate::registry` is not cfg-gated, so a
    /// payments-excised nest still returns a `payments` row — an app that
    /// folded the status reply alone would render an affordance for a plane the
    /// artifact does not carry. Leaving the two calls to seven separate app
    /// sessions is leaving seven chances to forget the second one.
    pub async fn rows(&self) -> Result<Vec<FeatureRow>, R::Error> {
        let capabilities = self.node_capabilities().await?;
        let reply = self.status().await?;
        Ok(feature_rows(&reply.features, &capabilities))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};

    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.features.status" => {
                fauna_protocol::encode_canonical(&FeaturesStatusReply::default())
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    /// The kind string and the empty payload are the whole wire contract of
    /// this call — a typo in either is a runtime `unknown kind`, which no type
    /// checks.
    #[test]
    fn status_sends_the_bare_kind_with_an_empty_request() {
        let requester = RecordingRequester::new(reply);
        let client = FeaturesClient::new(&requester);

        let got = block_on(client.status()).expect("status");
        assert!(got.features.is_empty());

        let (kind, payload) = requester.recorded();
        assert_eq!(kind, "fauna.features.status");
        let back: FeaturesStatusRequest =
            fauna_protocol::decode_strict(&payload).expect("decode request");
        assert_eq!(back, FeaturesStatusRequest::default());
    }
}
