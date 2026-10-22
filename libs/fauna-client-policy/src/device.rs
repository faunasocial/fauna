//! A rule document's bundled `wasm` labelers, run on the reader's device —
//! a nest's document on a member's, a guardian's on the ward's — through the
//! one shared runner the region plane runs a region document's scorers with
//! (`fauna_labeler::region`; `nest-rules.md` § The nest content policy: a rule
//! runs where the plaintext legitimately is).
//!
//! Compiled on first use and again whenever the document changes (a nest's
//! accepted sequence moves, the guardian hands a different document); one
//! answer kept per item id, since a scorer is deterministic over the same
//! item. A scorer that fails to compile, traps or says nothing adds no factor
//! — exactly the region plane's posture.

use std::cell::RefCell;
use std::collections::BTreeMap;

use fauna_core::content_category::ContentLabelEntry;
use fauna_core::content_policy::{ContentPolicyDocument, Publisher, ScorerKind};
use fauna_core::scoring::LabelerPostInput;
use fauna_labeler::region::{PreparedScorer, scorers_in_use};

use crate::nest_rules::AcceptedNestRules;

/// The most item answers kept per document before the cache starts over — a
/// bound on memory, not on correctness: a dropped answer is recomputed.
const MAX_CACHED_ITEMS: usize = 4096;

/// One document's compiled labelers and their answers per item id.
struct Prepared {
    scorers: Vec<PreparedScorer>,
    answers: BTreeMap<[u8; 32], Vec<ContentLabelEntry>>,
}

impl Prepared {
    /// Compile the `wasm` labelers some rule of `document` reads. A nest or
    /// guardian document bundling a `list` is malformed, so only modules
    /// arrive here.
    fn new(document: &ContentPolicyDocument, publisher: &Publisher) -> Self {
        let scorers = scorers_in_use(document, publisher)
            .into_iter()
            .filter(|scorer| scorer.kind == ScorerKind::Wasm)
            .filter_map(|scorer| PreparedScorer::prepare(publisher, scorer).ok())
            .collect();
        Self {
            scorers,
            answers: BTreeMap::new(),
        }
    }

    /// The factors the labelers produce for one item.
    fn labels(
        &mut self,
        content_id: Option<&[u8; 32]>,
        input: &LabelerPostInput,
    ) -> Vec<ContentLabelEntry> {
        if self.scorers.is_empty() {
            return Vec::new();
        }
        if let Some(id) = content_id
            && let Some(answer) = self.answers.get(id)
        {
            return answer.clone();
        }
        // An item with no id is scored, never cached: the all-zero key names
        // nothing real.
        let key = content_id.copied().unwrap_or([0; 32]);
        let answer: Vec<ContentLabelEntry> = self
            .scorers
            .iter()
            .filter_map(|scorer| match scorer.score(&key, input) {
                Ok(Some(permille)) => Some(ContentLabelEntry {
                    category: scorer.factor.clone(),
                    confidence_per_mille: permille,
                }),
                Ok(None) | Err(_) => None,
            })
            .collect();
        if let Some(id) = content_id {
            if self.answers.len() >= MAX_CACHED_ITEMS {
                self.answers.clear();
            }
            self.answers.insert(*id, answer.clone());
        }
        answer
    }
}

/// Per nest identity: the compiled labelers of one accepted sequence, and
/// their answers per item id.
#[derive(Default)]
pub(crate) struct NestScorers(RefCell<BTreeMap<String, (u64, Prepared)>>);

impl NestScorers {
    /// The factors the nest's `wasm` labelers produce for one item.
    pub(crate) fn labels(
        &self,
        accepted: &AcceptedNestRules,
        content_id: Option<&[u8; 32]>,
        input: &LabelerPostInput,
    ) -> Vec<ContentLabelEntry> {
        let prepare = || {
            let publisher = Publisher::Nest(accepted.domain.clone());
            (
                accepted.sequence,
                Prepared::new(&accepted.document, &publisher),
            )
        };
        let mut by_nest = self.0.borrow_mut();
        let entry = by_nest
            .entry(accepted.nest_id().to_string())
            .or_insert_with(prepare);
        if entry.0 != accepted.sequence {
            *entry = prepare();
        }
        entry.1.labels(content_id, input)
    }

    /// Drop a nest's compiled labelers — the device signed out of it.
    pub(crate) fn forget(&mut self, nest_id: &str) {
        self.0.get_mut().remove(nest_id);
    }
}

/// The guardian document's compiled labelers. Holds no key: its owner drops
/// them ([`Self::reset`]) whenever it is handed a different document.
#[derive(Default)]
pub(crate) struct GuardianScorers(RefCell<Option<Prepared>>);

impl GuardianScorers {
    /// The factors the guardian document's `wasm` labelers produce for one
    /// item.
    pub(crate) fn labels(
        &self,
        document: &ContentPolicyDocument,
        content_id: Option<&[u8; 32]>,
        input: &LabelerPostInput,
    ) -> Vec<ContentLabelEntry> {
        self.0
            .borrow_mut()
            .get_or_insert_with(|| Prepared::new(document, &Publisher::Guardian))
            .labels(content_id, input)
    }

    /// Forget the compiled labelers — the document they were compiled from is
    /// no longer the one in force.
    pub(crate) fn reset(&mut self) {
        *self.0.get_mut() = None;
    }

    /// Whether labelers are compiled now — for tests of the recompile rule.
    #[cfg(test)]
    pub(crate) fn is_prepared(&self) -> bool {
        self.0.borrow().is_some()
    }
}

/// Derived state shared by both caches: a copy starts with nothing compiled
/// (it compiles on first use), two are equal by what their owner holds, and
/// the debug form shows only how much is compiled.
macro_rules! derived_cache {
    ($ty:ident, $len:expr) => {
        impl Clone for $ty {
            fn clone(&self) -> Self {
                Self::default()
            }
        }

        impl PartialEq for $ty {
            fn eq(&self, _: &Self) -> bool {
                true
            }
        }

        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($ty))
                    .field("prepared", &$len(self))
                    .finish()
            }
        }
    };
}

derived_cache!(NestScorers, |s: &NestScorers| s.0.borrow().len());
derived_cache!(GuardianScorers, |s: &GuardianScorers| usize::from(
    s.0.borrow().is_some()
));

/// A labeler module (WAT text, which the runner compiles) that ignores its
/// input and answers one label — the ABI's `[len][BARE Vec<Label>]` output,
/// the BARE written by hand: a one-element list, the category string, the
/// little-endian `f64` confidence, the `TextAnalysis` source tag.
#[cfg(test)]
pub(crate) fn fixed_label_module(category: &str, confidence: f64) -> Vec<u8> {
    let mut payload = vec![1u8, u8::try_from(category.len()).unwrap()];
    payload.extend_from_slice(category.as_bytes());
    payload.extend_from_slice(&confidence.to_le_bytes());
    payload.push(1);
    let mut blob = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
    blob.extend_from_slice(&payload);
    let escaped: String = blob.iter().map(|b| format!("\\{b:02x}")).collect();
    format!(
        "(module (memory (export \"memory\") 2) \
         (data (i32.const 1024) \"{escaped}\") \
         (func (export \"alloc\") (param i32) (result i32) (i32.const 65536)) \
         (func (export \"label\") (param i32 i32) (result i32) (i32.const 1024)))"
    )
    .into_bytes()
}
