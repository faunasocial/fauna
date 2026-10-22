//! The ward's side of the guardian's rule document: the one join that runs
//! the document's scorers over an item on the ward's device.
//!
//! Owner doc: `docs/goal/behavior/nest-rules.md` § The shared rule editor (b)
//! — the guardian's document, whose rules the engine's guardian source folds
//! (`fauna_core::obligation::rules_from_guardian_policy`). Those rules name
//! factors in the guardian's namespace that no label carries; this join
//! produces them, so each app makes one call and folds the result: the
//! document's `keywords` scorers over the item's plaintext, and, with the
//! `scorers` feature, its `wasm` labelers over the whole scorer input through
//! the runner a nest document's labelers use ([`crate::NestRulesBook`]).

use std::borrow::Cow;

use fauna_core::content_category::ContentLabelEntry;
use fauna_core::content_policy::{ContentPolicyDocument, Publisher, evaluate_keyword_scorers};
use fauna_core::obligation::ContentPolicy;
use fauna_core::scoring::LabelerPostInput;

use crate::nest_rules::join_extra;

/// The guardian document in force on the ward's device, its labelers compiled
/// on first use and again only when a different document is handed over.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GuardianRules {
    document: Option<ContentPolicyDocument>,
    #[cfg(feature = "scorers")]
    device: crate::device::GuardianScorers,
}

impl GuardianRules {
    /// Hand over the supervised viewer's guardian policy — the same value the
    /// engine's `ContentPolicyState::set_ward_content_policy` takes, on each
    /// `fauna.family.status` read. The same document again keeps what is
    /// compiled; a different one, or none, drops it.
    pub fn set_policy(&mut self, policy: Option<&ContentPolicy>) {
        let document = policy.and_then(|p| p.document.as_ref());
        if self.document.as_ref() == document {
            return;
        }
        self.document = document.cloned();
        #[cfg(feature = "scorers")]
        self.device.reset();
    }

    /// An item's `labels` with the factors the guardian document's scorers
    /// produce over it joined — the input the composed call folds, so a
    /// guardian's keyword and labeler rules bind on the ward's device. Its
    /// labelers' answers are kept per `content_id`. Borrows `labels` when the
    /// ward has no guardian document or nothing is added; `input` is built
    /// only when there is a document.
    pub fn join_labels<'a>(
        &self,
        content_id: Option<&[u8; 32]>,
        labels: &'a [ContentLabelEntry],
        input: impl FnOnce() -> LabelerPostInput,
    ) -> Cow<'a, [ContentLabelEntry]> {
        let Some(document) = &self.document else {
            return Cow::Borrowed(labels);
        };
        let input = input();
        // Only the `scorers` leg adds to it.
        #[cfg_attr(not(feature = "scorers"), allow(unused_mut))]
        let mut extra = evaluate_keyword_scorers(
            document,
            &Publisher::Guardian,
            input.text.as_deref().unwrap_or_default(),
        );
        #[cfg(feature = "scorers")]
        extra.extend(self.device.labels(document, content_id, &input));
        #[cfg(not(feature = "scorers"))]
        let _ = content_id;
        join_extra(labels, extra)
    }
}

#[cfg(test)]
mod tests;
