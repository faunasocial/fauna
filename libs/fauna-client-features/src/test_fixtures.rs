//! Shared test-fixture row construction, behind the `test-fixtures` feature
//! (mirrors `fauna-media::test_fixtures`'s gating shape). `zaps_row` was
//! hand-copied byte-for-byte between fauna-tui (`nostr.rs`) and fauna-linux
//! (`settings/nostr_tab.rs`) — the linux copy's own doc comment already said
//! "Mirrors tui's `zaps_row` test fixture exactly." This is the one canonical
//! body. `item` was the same story one layer down: `fauna-tui`
//! (`settings/root.rs`) and `fauna-ffi` (`src/features.rs`) each hand-built
//! the identical bare `FeatureStatusItem`.

use fauna_core::feature_gate::{FeaturePolicy, RuleTier, UsageCounters, effective_policy};
use fauna_protocol::features::FeatureStatusItem;

use crate::{FeatureRow, GatedFeature, feature_row};

/// A bare `FeatureStatusItem` for `feature`, authored by `authored` rules and
/// no live usage — never hand-built, so a test exercises the same policy
/// resolution the page reads.
pub fn item(feature: GatedFeature, authored: &[(RuleTier, FeaturePolicy)]) -> FeatureStatusItem {
    FeatureStatusItem {
        feature,
        policy: effective_policy(feature, authored, &[]),
        usage: UsageCounters::default(),
        extra: Default::default(),
    }
}

/// A `zaps` row as the shared crate composes it — never hand-built, so a
/// test exercises the same `affordance` the page reads.
pub fn zaps_row(authored: &[(RuleTier, FeaturePolicy)]) -> FeatureRow {
    feature_row(&item(GatedFeature::Zaps, authored), &[])
}
