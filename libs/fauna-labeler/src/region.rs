//! Running a region content-policy document's bundled scorers
//! (`region-blocking.md` § The content plane → *The policy document*, `scorers`).
//!
//! `fauna_core::region_policy` validates a bundled scorer structurally but can
//! never run one: this crate depends on `fauna-core`, so running the sandbox from
//! there would be a dependency cycle. The positions that already link this crate
//! — the nest's public web render (the nest-as-publisher leg) and, later, an
//! app's shared layer — prepare and run scorers through here instead of each
//! growing its own loop, so the list lookup, the sandbox bounds and the factor a
//! scorer's output carries are spelled once.
//!
//! A scorer produces the factor `region:<region>/<name>` for one item, and that
//! factor joins the item's label list before the region fold. It says nothing
//! about an item when a `list` does not name the item or a `wasm` module emits no
//! label — and then no factor joins, so no rule on it can fire.

use anyhow::{Context, Result};
use fauna_core::region_authority::RegionCode;
use fauna_core::region_policy::{BundledScorer, ContentPolicyDocument, ScorerKind, scorer_factor};
use fauna_core::scoring::{LabelerListArtifact, LabelerPostInput, validate_list_artifact};

use crate::{
    LABELER_HOST_MAX_CPU_MICROSECONDS, LABELER_HOST_MAX_MEMORY_BYTES, LabelerRuntime, LoadedLabeler,
};

/// One bundled scorer made ready to run over many items: a list decoded once, a
/// module compiled once.
pub struct PreparedScorer {
    /// The scorer's name within its document.
    pub name: String,
    /// The factor its output carries, `region:<region>/<name>`.
    pub factor: String,
    prepared: Prepared,
}

enum Prepared {
    List(LabelerListArtifact),
    /// Bounded by the host ceilings, never by anything the document declares:
    /// an authority's scorer carries no resource limits of its own, and the
    /// crate's constants are the per-scorer bound the design names.
    Wasm {
        runtime: LabelerRuntime,
        module: LoadedLabeler,
    },
}

impl PreparedScorer {
    /// Decode (a `list`) or compile (a `wasm` module) one bundled scorer.
    ///
    /// # Errors
    ///
    /// The list does not validate, or the module does not compile. The
    /// document's own validation already refused a malformed list, so the list
    /// arm fails only on a document that skipped it.
    pub fn prepare(region: &RegionCode, scorer: &BundledScorer) -> Result<Self> {
        let prepared = match scorer.kind {
            ScorerKind::List => Prepared::List(
                validate_list_artifact(&scorer.bytes)
                    .with_context(|| format!("bundled list scorer {:?}", scorer.name))?,
            ),
            ScorerKind::Wasm => {
                let runtime = LabelerRuntime::new(
                    LABELER_HOST_MAX_MEMORY_BYTES,
                    LABELER_HOST_MAX_CPU_MICROSECONDS,
                )?;
                let module = runtime
                    .load_module(&scorer.bytes)
                    .with_context(|| format!("bundled wasm scorer {:?}", scorer.name))?;
                Prepared::Wasm { runtime, module }
            }
        };
        Ok(Self {
            name: scorer.name.clone(),
            factor: scorer_factor(region, &scorer.name),
            prepared,
        })
    }

    /// Score one item: its per-mille for this scorer's factor, or `None` when
    /// the scorer says nothing about it.
    ///
    /// `content_id` is the item's 32-byte id — the key a `list` is looked up by;
    /// `input` is what a `wasm` module reads. Deterministic: the same scorer over
    /// the same item always answers the same, which is what lets a caller cache
    /// an answer per (item, scorer bytes).
    ///
    /// # Errors
    ///
    /// The module traps, runs out of fuel or memory, or emits output that is not
    /// a BARE `Vec<Label>`.
    pub fn score(&self, content_id: &[u8; 32], input: &LabelerPostInput) -> Result<Option<u16>> {
        match &self.prepared {
            // Validated strictly ascending by id, so a binary search is exact.
            Prepared::List(list) => Ok(list
                .entries
                .binary_search_by(|e| e.content_id.as_slice().cmp(content_id.as_slice()))
                .ok()
                .map(|i| per_mille(list.entries[i].score))),
            Prepared::Wasm { runtime, module } => {
                let input = serde_bare::to_vec(input).context("encode labeler input (BARE)")?;
                let labels = runtime.execute_labels(module, &input)?;
                if labels.is_empty() {
                    return Ok(None);
                }
                let entry =
                    fauna_core::scoring::labels_to_score_entry(&labels, self.factor.clone(), 0);
                Ok(Some(per_mille(entry.score)))
            }
        }
    }
}

/// Clamp a validated-or-canonicalized score into the engine's `u16` per-mille.
fn per_mille(score: i64) -> u16 {
    score.clamp(0, 1000) as u16
}

/// The scorers of `document` that some rule of it actually reads — a scorer no
/// rule names produces a factor nothing folds, so it is never run. A document
/// that does not apply (an unimplemented grammar version, or malformed) has no
/// rule in force and so contributes none.
pub fn scorers_in_use<'d>(
    document: &'d ContentPolicyDocument,
    region: &RegionCode,
) -> Vec<&'d BundledScorer> {
    if !document.status(region).is_applied() {
        return Vec::new();
    }
    document
        .scorers
        .iter()
        .filter(|scorer| {
            let factor = scorer_factor(region, &scorer.name);
            document.rules.iter().any(|rule| rule.factor == factor)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fauna_core::region_policy::{ContentRule, ContentVerdict, REASON_DEFAULT_KEY};
    use fauna_core::scoring::{Label, LabelSource, build_list_artifact};

    use super::*;

    fn region() -> RegionCode {
        RegionCode::parse("NO").unwrap()
    }

    fn input(text: &str) -> LabelerPostInput {
        LabelerPostInput {
            text: Some(text.into()),
            hashtags: Vec::new(),
            has_media: false,
            media_type: None,
            duration_ms: None,
            author: fauna_core::identity::ActorId([7u8; 32]),
        }
    }

    fn list_scorer(name: &str, entries: Vec<([u8; 32], i64)>) -> BundledScorer {
        BundledScorer {
            name: name.into(),
            kind: ScorerKind::List,
            bytes: build_list_artifact(None, entries).unwrap(),
            extra: BTreeMap::new(),
        }
    }

    /// A module that ignores its input and returns a fixed `[len][payload]`
    /// output — the ABI shape `execute` reads (the fauna-ffi tests' fixture).
    fn fixed_output_scorer(name: &str, labels: &[Label]) -> BundledScorer {
        let payload = if labels.is_empty() {
            Vec::new()
        } else {
            serde_bare::to_vec(&labels.to_vec()).unwrap()
        };
        let mut blob = (payload.len() as u32).to_le_bytes().to_vec();
        blob.extend_from_slice(&payload);
        let escaped: String = blob.iter().map(|b| format!("\\{b:02x}")).collect();
        let wat = format!(
            "(module (memory (export \"memory\") 2) \
             (data (i32.const 1024) \"{escaped}\") \
             (func (export \"alloc\") (param i32) (result i32) (i32.const 65536)) \
             (func (export \"label\") (param i32 i32) (result i32) (i32.const 1024)))"
        );
        BundledScorer {
            name: name.into(),
            kind: ScorerKind::Wasm,
            bytes: wat.into_bytes(),
            extra: BTreeMap::new(),
        }
    }

    fn rule(factor: &str) -> ContentRule {
        ContentRule {
            factor: factor.into(),
            min_permille: 500,
            verdict: ContentVerdict::Block,
            reason_code: "R-1".into(),
            reason: BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), "Restricted.".into())]),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn a_list_scorer_answers_only_for_the_ids_it_names() {
        let named = [3u8; 32];
        let scorer = list_scorer("banned", vec![([1u8; 32], 100), (named, 900)]);
        let prepared = PreparedScorer::prepare(&region(), &scorer).unwrap();
        assert_eq!(prepared.factor, "region:NO/banned");
        assert_eq!(prepared.score(&named, &input("x")).unwrap(), Some(900));
        assert_eq!(
            prepared.score(&[2u8; 32], &input("x")).unwrap(),
            None,
            "an id the list does not name gets no factor at all, not a zero"
        );
    }

    #[test]
    fn a_wasm_scorer_answers_its_strongest_label_and_nothing_for_no_label() {
        let hit = fixed_output_scorer(
            "model",
            &[
                Label {
                    category: "a".into(),
                    confidence: 0.4,
                    source: LabelSource::TextAnalysis,
                },
                Label {
                    category: "b".into(),
                    confidence: 0.75,
                    source: LabelSource::TextAnalysis,
                },
            ],
        );
        let prepared = PreparedScorer::prepare(&region(), &hit).unwrap();
        assert_eq!(prepared.score(&[0u8; 32], &input("x")).unwrap(), Some(750));

        let silent = fixed_output_scorer("quiet", &[]);
        let prepared = PreparedScorer::prepare(&region(), &silent).unwrap();
        assert_eq!(prepared.score(&[0u8; 32], &input("x")).unwrap(), None);
    }

    #[test]
    fn a_module_that_does_not_compile_is_refused_at_prepare() {
        let broken = BundledScorer {
            name: "broken".into(),
            kind: ScorerKind::Wasm,
            bytes: b"not a module".to_vec(),
            extra: BTreeMap::new(),
        };
        assert!(PreparedScorer::prepare(&region(), &broken).is_err());
    }

    #[test]
    fn only_scorers_some_rule_reads_are_in_use() {
        let doc = ContentPolicyDocument {
            version: fauna_core::region_policy::GRAMMAR_VERSION,
            rules: vec![rule("region:NO/read"), rule("nsfw")],
            scorers: vec![
                list_scorer("read", vec![([1u8; 32], 900)]),
                list_scorer("unread", vec![([1u8; 32], 900)]),
            ],
            extra: BTreeMap::new(),
        };
        let names: Vec<&str> = scorers_in_use(&doc, &region())
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, ["read"]);
    }

    #[test]
    fn a_document_that_does_not_apply_runs_no_scorer() {
        let doc = ContentPolicyDocument {
            version: fauna_core::region_policy::GRAMMAR_VERSION + 1,
            rules: vec![rule("region:NO/read")],
            scorers: vec![list_scorer("read", vec![([1u8; 32], 900)])],
            extra: BTreeMap::new(),
        };
        assert!(scorers_in_use(&doc, &region()).is_empty());
    }
}
