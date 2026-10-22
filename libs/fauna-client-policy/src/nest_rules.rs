//! The member's side of a nest's **published rules**: the book that keeps the
//! last-accepted document per nest, and the read-only summary the joining and
//! settings surfaces paint.
//!
//! Owner doc: `docs/goal/behavior/nest-rules.md` § The nest content policy →
//! *Carriage* and § Acceptance at joining, and a change of rules. A nest's
//! self-description carries its document signed with the nest's identity key;
//! the device verifies it through the one door
//! ([`fauna_core::content_policy::verify_nest_policy`]), keeps the
//! last-accepted document per nest with its sequence, and so can tell a change
//! (a newer sequence → the one-line notice naming the nest) from a repeat. An
//! answer that does not verify, replays an older sequence, or leaves the
//! document out leaves the last-known rules in force — the region plane's fail
//! posture, for the same reasons.
//!
//! Every decision is here so each app only paints (priority #2): the book
//! hands the render engine its [`NestRuleSet`]s and the surfaces a
//! [`NestRulesSummary`] whose every label is already chosen.

use std::borrow::Cow;
use std::collections::BTreeMap;

use fauna_core::content_category::ContentLabelEntry;
use fauna_core::content_policy::{
    ContentPolicyDocument, NestPolicyRejection, PolicyStatus, Publisher, evaluate_keyword_scorers,
    reason_text, verify_nest_policy,
};
use fauna_core::localized::LocalizedText;
use fauna_core::obligation::{NestRuleSet, rules_from_nest_policy};
use fauna_core::scoring::LabelerPostInput;
use serde::{Deserialize, Serialize};

use crate::{RuleEditor, RuleKind};

/// Whether this build runs a document's `wasm` labelers on the device — the
/// `scorers` feature. Where it does not (the store-safe FFI flavor, which
/// carries no interpreter), a labeler rule adds no factor, so the member's
/// summary names it *not applied on this device* as it names a prompt rule
/// (`nest-rules.md` decision 1). The editor does not: the device that writes
/// a rule is not the one that enforces it.
const LABELERS_RUN_ON_THIS_DEVICE: bool = cfg!(feature = "scorers");

/// A nest's signed content policy as its self-description carries it — the
/// fields of `fauna_protocol::content_policy::SignedContentPolicy`, borrowed,
/// so this crate stays off the wire crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedNestRules<'a> {
    /// The document's canonical bytes, exactly as signed.
    pub document: &'a [u8],
    pub sequence: u64,
    pub issued_at: i64,
    pub signature: &'a [u8],
}

/// One accepted signed record, in the form an app persists between runs
/// ([`NestRulesBook::stored`] / [`NestRulesBook::restore`]). Kept signed, so a
/// restore verifies again rather than trusting the app's storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct StoredNestRules {
    /// 64-char lowercase hex of the nest's identity key.
    pub nest_id: String,
    /// The nest's domain as its self-description named it.
    pub domain: String,
    pub document: Vec<u8>,
    pub sequence: u64,
    pub issued_at: i64,
    pub signature: Vec<u8>,
}

/// One nest's rules as last accepted.
#[derive(Debug, Clone, PartialEq)]
pub struct AcceptedNestRules {
    pub domain: String,
    pub sequence: u64,
    pub issued_at: i64,
    pub document: ContentPolicyDocument,
    /// What the composed render call folds for items read through this nest.
    pub rule_set: NestRuleSet,
    record: StoredNestRules,
}

impl AcceptedNestRules {
    /// The nest's identity, 64-char lowercase hex.
    #[cfg(feature = "scorers")]
    pub(crate) fn nest_id(&self) -> &str {
        &self.record.nest_id
    }

    /// The read-only rows the joining and settings surfaces paint, in `lang`.
    pub fn summary(&self, lang: &str) -> NestRulesSummary {
        summary(
            &self.domain,
            self.sequence,
            self.issued_at,
            &self.document,
            lang,
        )
    }
}

/// What one read of a nest's self-description did to the book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NestRulesRead {
    /// The nest has published no rules and none were known.
    NoRules,
    /// The first rules this device knows for the nest. The joining surface
    /// shows them; there is no change to announce.
    First { sequence: u64 },
    /// The rules already in force.
    Unchanged,
    /// A newer sequence than the last accepted: show the one-line notice
    /// naming the nest (`nest_rules.changed_notice`). The new rules bind from
    /// this read.
    Changed { domain: String, sequence: u64 },
    /// Refused; the last-known rules, if any, stay in force.
    Refused(NestRulesRefusal),
}

/// Why a read was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NestRulesRefusal {
    /// The self-description's `nest_id` is not 64 hex characters.
    BadNestId,
    /// The signature or the document failed the one door.
    Rejected(NestPolicyRejection),
    /// An older sequence than the one in force — a replay.
    Replay { known: u64, offered: u64 },
    /// The sequence in force, with different bytes.
    Conflict { sequence: u64 },
    /// The document is gone from a nest whose rules this device knows. A nest
    /// withdraws its rules by publishing an empty document, never by silence.
    Missing { known: u64 },
}

/// The last-accepted rules of every nest this device reads through, keyed by
/// the nest's identity.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NestRulesBook {
    by_nest: BTreeMap<String, AcceptedNestRules>,
    /// The accepted documents' `wasm` labelers, compiled on first use.
    #[cfg(feature = "scorers")]
    device: crate::device::NestScorers,
}

impl NestRulesBook {
    /// Read one self-description: `nest_id` and `domain` as it names the nest,
    /// `signed` its `moderation.content_policy`.
    pub fn read(
        &mut self,
        nest_id: &str,
        domain: &str,
        signed: Option<SignedNestRules<'_>>,
    ) -> NestRulesRead {
        let Some(key) = nest_key(nest_id) else {
            return NestRulesRead::Refused(NestRulesRefusal::BadNestId);
        };
        let id = hex_lower(&key);
        let known = self.by_nest.get(&id);
        let Some(signed) = signed else {
            return match known {
                Some(k) => NestRulesRead::Refused(NestRulesRefusal::Missing { known: k.sequence }),
                None => NestRulesRead::NoRules,
            };
        };
        if let Some(k) = known {
            if signed.sequence < k.sequence {
                return NestRulesRead::Refused(NestRulesRefusal::Replay {
                    known: k.sequence,
                    offered: signed.sequence,
                });
            }
            if signed.sequence == k.sequence {
                let same =
                    k.record.document == signed.document && k.record.issued_at == signed.issued_at;
                return if same {
                    NestRulesRead::Unchanged
                } else {
                    NestRulesRead::Refused(NestRulesRefusal::Conflict {
                        sequence: k.sequence,
                    })
                };
            }
        }
        let accepted = match accept(&key, &id, domain, signed) {
            Ok(accepted) => accepted,
            Err(e) => return NestRulesRead::Refused(NestRulesRefusal::Rejected(e)),
        };
        let first = known.is_none();
        let sequence = accepted.sequence;
        self.by_nest.insert(id, accepted);
        if first {
            NestRulesRead::First { sequence }
        } else {
            NestRulesRead::Changed {
                domain: domain.to_string(),
                sequence,
            }
        }
    }

    /// The rules in force for the nest `nest_id` names.
    pub fn get(&self, nest_id: &str) -> Option<&AcceptedNestRules> {
        let key = nest_key(nest_id)?;
        self.by_nest.get(&hex_lower(&key))
    }

    /// The rule sets the composed render call folds for an item read through
    /// the nest `nest_id` names — one, or none when it published no rules.
    pub fn rule_sets_for(&self, nest_id: &str) -> Vec<NestRuleSet> {
        self.get(nest_id)
            .map(|a| vec![a.rule_set.clone()])
            .unwrap_or_default()
    }

    /// An item's `labels` with the factors the nest's bundled scorers produce
    /// over it joined — the input the composed call folds for an item read
    /// through the nest `nest_id` names (`nest-rules.md` § The nest content
    /// policy: rules run post-decrypt on the device). Its `keywords` scorers
    /// match `input`'s text; with the `scorers` feature its `wasm` labelers run
    /// over `input` through the one shared runner the region plane uses
    /// (`fauna_labeler::region`), compiled once per accepted sequence, their
    /// answers kept per `content_id`. Borrows `labels` when nothing is added;
    /// `input` is built only when the nest has rules.
    pub fn join_labels<'a>(
        &self,
        nest_id: &str,
        content_id: Option<&[u8; 32]>,
        labels: &'a [ContentLabelEntry],
        input: impl FnOnce() -> LabelerPostInput,
    ) -> Cow<'a, [ContentLabelEntry]> {
        let Some(accepted) = self.get(nest_id) else {
            return Cow::Borrowed(labels);
        };
        let publisher = Publisher::Nest(accepted.domain.clone());
        let input = input();
        // Only the `scorers` leg adds to it.
        #[cfg_attr(not(feature = "scorers"), allow(unused_mut))]
        let mut extra = evaluate_keyword_scorers(
            &accepted.document,
            &publisher,
            input.text.as_deref().unwrap_or_default(),
        );
        #[cfg(feature = "scorers")]
        extra.extend(self.device.labels(accepted, content_id, &input));
        #[cfg(not(feature = "scorers"))]
        let _ = content_id;
        join_extra(labels, extra)
    }

    /// Drop a nest's rules — the device signed out of it.
    pub fn forget(&mut self, nest_id: &str) {
        if let Some(key) = nest_key(nest_id) {
            let id = hex_lower(&key);
            #[cfg(feature = "scorers")]
            self.device.forget(&id);
            self.by_nest.remove(&id);
        }
    }

    /// Every accepted record, for the app to persist.
    pub fn stored(&self) -> Vec<StoredNestRules> {
        self.by_nest.values().map(|a| a.record.clone()).collect()
    }

    /// Rebuild the book from persisted records, verifying each again. A record
    /// that no longer verifies is dropped: the next self-description read
    /// supplies the nest's rules afresh.
    pub fn restore(records: Vec<StoredNestRules>) -> Self {
        let mut book = Self::default();
        for record in records {
            let Some(key) = nest_key(&record.nest_id) else {
                continue;
            };
            let id = hex_lower(&key);
            let signed = SignedNestRules {
                document: &record.document,
                sequence: record.sequence,
                issued_at: record.issued_at,
                signature: &record.signature,
            };
            if let Ok(accepted) = accept(&key, &id, &record.domain, signed) {
                let newer = book
                    .by_nest
                    .get(&id)
                    .is_none_or(|k| accepted.sequence > k.sequence);
                if newer {
                    book.by_nest.insert(id, accepted);
                }
            }
        }
        book
    }
}

/// `labels` with `extra` appended — borrowed when there is nothing to add.
pub(crate) fn join_extra(
    labels: &[ContentLabelEntry],
    extra: Vec<ContentLabelEntry>,
) -> Cow<'_, [ContentLabelEntry]> {
    if extra.is_empty() {
        return Cow::Borrowed(labels);
    }
    let mut joined = labels.to_vec();
    joined.extend(extra);
    Cow::Owned(joined)
}

fn accept(
    key: &[u8; 32],
    id: &str,
    domain: &str,
    signed: SignedNestRules<'_>,
) -> Result<AcceptedNestRules, NestPolicyRejection> {
    let document = verify_nest_policy(
        key,
        signed.document,
        signed.sequence,
        signed.issued_at,
        signed.signature,
    )?;
    Ok(AcceptedNestRules {
        domain: domain.to_string(),
        sequence: signed.sequence,
        issued_at: signed.issued_at,
        rule_set: rules_from_nest_policy(&document, domain),
        document,
        record: StoredNestRules {
            nest_id: id.to_string(),
            domain: domain.to_string(),
            document: signed.document.to_vec(),
            sequence: signed.sequence,
            issued_at: signed.issued_at,
            signature: signed.signature.to_vec(),
        },
    })
}

fn nest_key(nest_id: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(nest_id).ok()
}

fn hex_lower(key: &[u8; 32]) -> String {
    fauna_core::hex32::encode(key)
}

/// A nest's rules as the joining surface (`onboarding-nest-rules`) and the
/// settings rules surface (`settings-nest-rules-*`) show them: per rule the
/// verdict, the level and the nest's own reason, every label already chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct NestRulesSummary {
    pub domain: String,
    pub sequence: u64,
    pub issued_at: i64,
    /// Empty for a nest with no rules — the joining surface then renders
    /// nothing.
    pub rules: Vec<NestRuleLine>,
    /// Why none of the document applies on this device; `None` when it
    /// applies.
    pub not_applied: Option<NestRulesNotApplied>,
}

/// Why none of a nest's document applies on this device. The member-facing
/// sentence for each is owed with the surfaces' strings (a string shown at
/// joining is shown before sign-in, so it is a public text —
/// `docs/goal/architecture/public-texts.md` § The boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum NestRulesNotApplied {
    /// A newer grammar than this build implements: updating the app applies
    /// them.
    NewerVersion,
    /// Structurally wrong — the nest validates before storing, so this is a
    /// defence, not an expected state.
    Malformed,
}

/// One rule of a [`NestRulesSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct NestRuleLine {
    pub verdict: LocalizedText,
    /// `None` where a level means nothing (a keywords rule).
    pub level: Option<LocalizedText>,
    /// The nest's own words in the member's language where the document has
    /// them, else its default. Shown verbatim, never translated.
    pub reason: String,
    /// "Not applied on this device" — a rule this build cannot enforce.
    pub inert: Option<LocalizedText>,
}

/// The summary of `document` as the nest at `domain` published it — the one
/// rendering the joining and settings surfaces share, read off the editor's
/// own view so a rule reads the same wherever it is shown.
pub fn summary(
    domain: &str,
    sequence: u64,
    issued_at: i64,
    document: &ContentPolicyDocument,
    lang: &str,
) -> NestRulesSummary {
    let publisher = Publisher::Nest(domain.to_string());
    let view = RuleEditor::load(Some(document.clone()), publisher.clone()).view();
    let not_applied = match document.status(&publisher) {
        PolicyStatus::Applied => None,
        PolicyStatus::InertUnimplementedVersion { .. } => Some(NestRulesNotApplied::NewerVersion),
        PolicyStatus::Malformed(_) => Some(NestRulesNotApplied::Malformed),
    };
    let rules = view
        .rows
        .iter()
        .zip(&document.rules)
        .map(|(row, rule)| NestRuleLine {
            verdict: row.verdict.label(),
            level: row
                .level_index
                .and_then(|i| view.levels.get(i as usize))
                .map(|l| l.label.clone()),
            reason: reason_text(&rule.reason, lang).to_string(),
            inert: row.inert.clone().or_else(|| {
                (row.kind == RuleKind::Labeler && !LABELERS_RUN_ON_THIS_DEVICE)
                    .then(|| LocalizedText::key("rule_editor.prompt_not_applied"))
            }),
        })
        .collect();
    NestRulesSummary {
        domain: domain.to_string(),
        sequence,
        issued_at,
        rules,
        not_applied,
    }
}

#[cfg(test)]
mod tests;
