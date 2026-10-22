//! UniFFI face of the member's **nest-rules book**
//! (`fauna_client_policy::NestRulesBook`; `docs/goal/behavior/nest-rules.md`
//! § The nest content policy → *Carriage*, § Acceptance at joining, and a
//! change of rules): the app hands each `fauna.nest.info` reply's nest id,
//! domain and `moderation.content_policy` to [`FfiNestRulesBook::read`], shows
//! the changed notice on [`FfiNestRulesRead::Changed`], passes
//! [`FfiNestRulesBook::rule_sets_for`] for the nest an item was read through
//! to `content_render_composed_with_nest`, and paints
//! [`FfiNestRulesBook::summary`] on the joining and settings surfaces. Every
//! decision is the shared book's; the web twin is `fauna-wasm`'s
//! `WasmNestRulesBook`.
//!
//! Gated `rule-editor` with the editor's face: the summary embeds bare
//! `LocalizedText`, which `uniffi-bindgen-go` cannot emit.

use std::sync::{Arc, Mutex};

use fauna_client_policy::{
    NestRulesBook, NestRulesRead, NestRulesSummary, SignedNestRules, StoredNestRules,
};

use crate::family::FfiNestRuleSet;
#[cfg(feature = "value-format")]
use crate::region::FfiRegionPlane;

/// A nest's signed content policy, as `SignedContentPolicy` carries it in the
/// self-description's `moderation.content_policy`.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSignedNestRules {
    /// The document's canonical bytes, exactly as signed.
    pub document: Vec<u8>,
    pub sequence: u64,
    pub issued_at: i64,
    pub signature: Vec<u8>,
}

/// What one read did — `fauna_client_policy::NestRulesRead`.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiNestRulesRead {
    /// The nest has published no rules and none were known.
    NoRules,
    /// The first rules this device knows for the nest; nothing to announce.
    First {
        sequence: u64,
    },
    Unchanged,
    /// Show the one-line changed notice naming `domain`
    /// (`nest_rules.changed_notice`).
    Changed {
        domain: String,
        sequence: u64,
    },
    /// Refused; the last-known rules stay in force. `reason` is for the log,
    /// never the member.
    Refused {
        reason: String,
    },
}

impl From<NestRulesRead> for FfiNestRulesRead {
    fn from(read: NestRulesRead) -> Self {
        match read {
            NestRulesRead::NoRules => Self::NoRules,
            NestRulesRead::First { sequence } => Self::First { sequence },
            NestRulesRead::Unchanged => Self::Unchanged,
            NestRulesRead::Changed { domain, sequence } => Self::Changed { domain, sequence },
            NestRulesRead::Refused(refusal) => Self::Refused {
                reason: format!("{refusal:?}"),
            },
        }
    }
}

/// One session-nest refresh: which nest was read, and what the read did.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiNestRulesRefresh {
    pub nest_id: String,
    pub domain: String,
    pub read: FfiNestRulesRead,
}

/// The book, held across the boundary — one per app, over every nest it is
/// signed into.
#[derive(uniffi::Object)]
pub struct FfiNestRulesBook {
    book: Mutex<NestRulesBook>,
}

// Helpers in a PLAIN impl: a `#[uniffi::export]` block exports its private
// methods too.
impl FfiNestRulesBook {
    fn with<T>(&self, f: impl FnOnce(&mut NestRulesBook) -> T) -> T {
        f(&mut self.book.lock().expect("nest rules book lock"))
    }

    /// The region plane's nest leg (`FfiRegionPlane::render_with_nest`): the
    /// item's labels with the nest's scorer factors joined, and the rule sets
    /// to fold, in one lock.
    #[cfg(feature = "value-format")]
    pub(crate) fn join_for_render(
        &self,
        nest_id: &str,
        content_id: Option<&[u8; 32]>,
        labels: &[fauna_core::content_category::ContentLabelEntry],
        input: impl FnOnce() -> fauna_core::scoring::LabelerPostInput,
    ) -> (
        Vec<fauna_core::content_category::ContentLabelEntry>,
        Vec<fauna_core::obligation::NestRuleSet>,
    ) {
        self.with(|book| {
            let joined = book
                .join_labels(nest_id, content_id, labels, input)
                .into_owned();
            (joined, book.rule_sets_for(nest_id))
        })
    }
}

/// The ONE call a surface makes for an item read through a nest
/// (`nest-rules.md` § The nest source in the engine) — the twin of the wasm
/// plane's `renderWithNest`.
#[cfg(feature = "value-format")]
#[uniffi::export]
impl FfiRegionPlane {
    /// `FfiRegionPlane::render` with the nest source composed in: the region,
    /// the nest `nest_id` names (its keyword and labeler factors joined over
    /// the item), the guardian document and floor, and the viewer's own
    /// thresholds, strictest-wins. The answer's `nest_placeholder` is set
    /// exactly when a nest rule drove a `block` or `collapse`; its reason is
    /// the admin's, verbatim, in `lang` where written.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_nest(
        &self,
        book: Arc<FfiNestRulesBook>,
        nest_id: String,
        labels: Vec<fauna_core::content_category::ContentLabelEntry>,
        content_policy: Option<crate::family::FfiContentPolicy>,
        own_spam_permille: Option<u16>,
        own_phishing_permille: Option<u16>,
        content_id_hex: Option<String>,
        author_hex: Option<String>,
        text: String,
        hashtags: Vec<String>,
        has_media: bool,
        lang: String,
    ) -> crate::region::FfiRegionRender {
        self.compose(
            Some((&book, nest_id.as_str())),
            labels,
            content_policy,
            own_spam_permille,
            own_phishing_permille,
            content_id_hex,
            author_hex,
            text,
            hashtags,
            has_media,
            lang,
        )
    }
}

#[uniffi::export]
impl FfiNestRulesBook {
    /// An empty book.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            book: Mutex::new(NestRulesBook::default()),
        })
    }

    /// The book rebuilt from [`Self::stored`]'s records, each verified again.
    #[uniffi::constructor]
    pub fn restore(records: Vec<StoredNestRules>) -> Arc<Self> {
        Arc::new(Self {
            book: Mutex::new(NestRulesBook::restore(records)),
        })
    }

    /// Read one self-description: its `nest_id`, `domain` and
    /// `moderation.content_policy`.
    pub fn read(
        &self,
        nest_id: String,
        domain: String,
        // Not named `signed`: UniFFI's C header would carry it as a parameter and
        // `signed` is a C keyword, which breaks the Apple header module.
        signed_rules: Option<FfiSignedNestRules>,
    ) -> FfiNestRulesRead {
        let signed = signed_rules.as_ref().map(|s| SignedNestRules {
            document: &s.document,
            sequence: s.sequence,
            issued_at: s.issued_at,
            signature: &s.signature,
        });
        self.with(|book| book.read(&nest_id, &domain, signed))
            .into()
    }

    /// The rule sets to fold for an item read through the nest `nest_id`
    /// names.
    pub fn rule_sets_for(&self, nest_id: String) -> Vec<FfiNestRuleSet> {
        self.with(|book| book.rule_sets_for(&nest_id))
            .into_iter()
            .map(Into::into)
            .collect()
    }

    /// An item's labels with the factors the nest's bundled scorers produce
    /// over it joined — what to pass as `labels` to
    /// `content_render_composed_with_nest` for an item read through the nest
    /// `nest_id` names. The scorer input is the region plane's
    /// (`FfiRegionPlane::render`): `content_id_hex` (the item's 32-byte id;
    /// `None` for an item without one), the author (hex actor id; `None` → the
    /// zero id), text, hashtags and whether it carries media. The keyword rules
    /// match `text`; the `wasm` labelers run where this build carries the
    /// `labeler` feature.
    #[allow(clippy::too_many_arguments)]
    pub fn join_labels(
        &self,
        nest_id: String,
        labels: Vec<fauna_core::content_category::ContentLabelEntry>,
        content_id_hex: Option<String>,
        author_hex: Option<String>,
        text: String,
        hashtags: Vec<String>,
        has_media: bool,
    ) -> Vec<fauna_core::content_category::ContentLabelEntry> {
        let id = content_id_hex
            .as_deref()
            .and_then(|h| fauna_core::hex32::decode(h).ok());
        self.with(|book| {
            book.join_labels(&nest_id, id.as_ref(), &labels, || {
                let author = author_hex
                    .as_deref()
                    .and_then(|h| fauna_core::hex32::decode(h).ok())
                    .map(fauna_core::identity::ActorId)
                    .unwrap_or(fauna_core::identity::ActorId([0; 32]));
                fauna_core::scoring::LabelerPostInput::from_card(
                    author, &text, &hashtags, has_media,
                )
            })
            .into_owned()
        })
    }

    /// The rules in force for the nest, as the joining and settings surfaces
    /// paint them in `lang`; `None` when it published none.
    pub fn summary(&self, nest_id: String, lang: String) -> Option<NestRulesSummary> {
        self.with(|book| book.get(&nest_id).map(|a| a.summary(&lang)))
    }

    /// Drop a nest's rules — the app signed out of it.
    pub fn forget(&self, nest_id: String) {
        self.with(|book| book.forget(&nest_id));
    }

    /// Every accepted record, for the app to persist.
    pub fn stored(&self) -> Vec<StoredNestRules> {
        self.with(|book| book.stored())
    }
}

#[fauna_uniffi_async::export]
impl FfiNestRulesBook {
    /// Read the session nest's self-description (`fauna.nest.info`, asked
    /// afresh past the connection's memo — a memo would answer with the reply a
    /// change just made stale) and fold it into the book: the one read the app
    /// makes at sign-in and on every `nest_info` push and reconnect. Show the
    /// changed notice on `FfiNestRulesRead::Changed`.
    pub async fn refresh(
        &self,
        nest: Arc<crate::FfiNestClient>,
    ) -> Result<FfiNestRulesRefresh, crate::FfiError> {
        let info: fauna_protocol::discovery::NestInfoReply = fauna_protocol::RpcRequester::request(
            nest.nest_arc().as_ref(),
            "fauna.nest.info",
            fauna_protocol::discovery::NestInfoRequest::default(),
        )
        .await
        .map_err(crate::stringify)?;
        let signed = info
            .moderation
            .content_policy
            .as_ref()
            .map(|p| SignedNestRules {
                document: &p.document,
                sequence: p.sequence,
                issued_at: p.issued_at,
                signature: &p.signature,
            });
        let read = self.with(|book| book.read(&info.nest_id, &info.domain, signed));
        Ok(FfiNestRulesRefresh {
            nest_id: info.nest_id,
            domain: info.domain,
            read: read.into(),
        })
    }
}

#[cfg(all(test, feature = "value-format"))]
mod tests {
    use std::collections::BTreeMap;

    use fauna_core::content_category::ContentLabelEntry;
    use fauna_core::content_policy::{
        BundledScorer, ContentPolicyDocument, ContentRule, ContentVerdict, Publisher,
        REASON_DEFAULT_KEY, ScorerKind, encode_document, scorer_factor, sign_nest_policy,
    };
    use fauna_core::identity::ActorKeypair;

    use super::*;
    use crate::family::FfiContentPolicy;

    const DOMAIN: &str = "example.test";

    /// One keywords rule over `term`, under `publisher`.
    fn keyword_document(publisher: &Publisher, term: &str, reason: &str) -> ContentPolicyDocument {
        let mut doc = ContentPolicyDocument::empty();
        doc.scorers.push(BundledScorer {
            name: "keywords".to_string(),
            kind: ScorerKind::Keywords,
            bytes: term.as_bytes().to_vec(),
            extra: BTreeMap::new(),
        });
        doc.rules.push(ContentRule {
            factor: scorer_factor(publisher, "keywords"),
            min_permille: 500,
            verdict: ContentVerdict::Block,
            reason_code: "rule-1".to_string(),
            reason: BTreeMap::from([(REASON_DEFAULT_KEY.to_string(), reason.to_string())]),
            extra: BTreeMap::new(),
        });
        doc
    }

    fn plane(name: &str) -> Arc<FfiRegionPlane> {
        let dir = std::env::temp_dir().join(format!(
            "fauna-ffi-nest-rules-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        FfiRegionPlane::open_pending(dir.to_string_lossy().into_owned())
    }

    fn render(
        plane: &FfiRegionPlane,
        book: &Arc<FfiNestRulesBook>,
        nest_id: &str,
        policy: Option<FfiContentPolicy>,
        text: &str,
    ) -> crate::region::FfiRegionRender {
        plane.render_with_nest(
            Arc::clone(book),
            nest_id.to_string(),
            Vec::<ContentLabelEntry>::new(),
            policy,
            None,
            None,
            None,
            None,
            text.to_string(),
            Vec::new(),
            false,
            "en".to_string(),
        )
    }

    #[test]
    fn render_with_nest_blocks_a_matching_item_naming_the_nest_and_its_reason() {
        // nest-rules.md § The nest source in the engine: the one call joins the
        // nest's keyword factor and folds its rule set beside the region's.
        let nest = ActorKeypair::from_secret([1; 32]);
        let doc = keyword_document(
            &Publisher::Nest(DOMAIN.to_string()),
            "casino",
            "No gambling",
        );
        let document = encode_document(&doc).unwrap();
        let signature = sign_nest_policy(nest.signing_key(), &document, 1, 1_700_000_001).unwrap();
        let book = FfiNestRulesBook::new();
        let read = book.read(
            nest.actor_id_hex(),
            DOMAIN.to_string(),
            Some(FfiSignedNestRules {
                document,
                sequence: 1,
                issued_at: 1_700_000_001,
                signature: signature.to_vec(),
            }),
        );
        assert_eq!(read, FfiNestRulesRead::First { sequence: 1 });
        let plane = plane("nest");

        let blocked = render(
            &plane,
            &book,
            &nest.actor_id_hex(),
            None,
            "Visit the CASINO",
        );
        assert_eq!(blocked.verdict, "block");
        assert!(blocked.placeholder.is_none(), "the region drove nothing");
        let placeholder = blocked.nest_placeholder.expect("the nest drove the block");
        assert_eq!(placeholder.verb, "block");
        assert_eq!(placeholder.domain, DOMAIN);
        assert_eq!(placeholder.reason, "No gambling");

        // An item the rule does not match, and the same item read through a
        // nest with no rules on the device, show and name no nest.
        let quiet = render(
            &plane,
            &book,
            &nest.actor_id_hex(),
            None,
            "a quiet afternoon",
        );
        assert_eq!(quiet.verdict, "show");
        assert!(quiet.nest_placeholder.is_none());
        let elsewhere = render(&plane, &book, "00", None, "Visit the CASINO");
        assert_eq!(elsewhere.verdict, "show");
        assert!(elsewhere.nest_placeholder.is_none());
    }

    #[test]
    fn the_guardian_documents_keyword_rule_joins_in_render_and_render_with_nest() {
        // nest-rules.md § The shared rule editor (b): the ward's guardian
        // document binds through the plane's own render, with no nest named
        // and with one.
        let doc = keyword_document(&Publisher::Guardian, "dare", "Not for you");
        let policy = FfiContentPolicy {
            nsfw: "inherit".into(),
            spam: "inherit".into(),
            phishing: "inherit".into(),
            commercial: "inherit".into(),
            document: Some(encode_document(&doc).unwrap()),
        };
        let plane = plane("guardian");
        let plain = plane.render(
            Vec::new(),
            Some(policy.clone()),
            None,
            None,
            None,
            None,
            "a new dare".to_string(),
            Vec::new(),
            false,
            "en".to_string(),
        );
        assert_eq!(plain.verdict, "block");
        assert!(
            plain.nest_placeholder.is_none(),
            "the guardian is not a nest"
        );

        let book = FfiNestRulesBook::new();
        let composed = render(&plane, &book, "00", Some(policy.clone()), "a new dare");
        assert_eq!(composed.verdict, "block");
        let other = render(&plane, &book, "00", Some(policy), "homework");
        assert_eq!(other.verdict, "show");
    }
}
