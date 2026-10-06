//! The nest-as-publisher leg of the region content plane (`region-blocking.md`
//! § The content plane → *The nest-as-publisher leg*).
//!
//! The one nest surface that evaluates content: the public web pages the nest
//! renders to the open internet from published posts, where the nest is the
//! publisher and no app exists to apply a region for the visitor. The nest
//! applies the content policy of its own admin-declared situs — every region on
//! the registry's parent chain, strictest-wins — through the SAME composed
//! engine the apps call, with the region source only: a public page has no
//! viewer thresholds and no guardian. Its inputs are the labels the nest already
//! holds for the post plus each bundled scorer's factor over the post's public
//! plaintext; for a gated post that is the public preview, never the sealed
//! full body.
//!
//! This module is the nest's only caller of `render_verdict_composed`
//! (`the_fold_has_exactly_one_caller` pins it): every in-app surface of the same
//! posts is untouched, because the viewer's own app applies the viewer's own
//! region.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use fauna_core::content_category::ContentLabelEntry;
use fauna_core::obligation::{RegionAttribution, RenderVerdict, render_verdict_composed};
use fauna_core::region_authority::{
    PAYLOAD_KIND_CONTENT_POLICY, PolicyArtifact, RegionRegistry, verify_artifact,
};
use fauna_core::region_policy::{
    BundledScorer, REASON_DEFAULT_KEY, RegionRuleSet, rules_from_region_policy,
};
use fauna_core::scoring::LabelerPostInput;
use fauna_labeler::region::{PreparedScorer, scorers_in_use};

use super::render::PostContext;
use crate::db::CacheDb;

/// A bundled scorer's cache identity: the digest of its bytes. A module is
/// deterministic, so one scorer's answer for one post never changes while both
/// stay the same — whichever document version or scorer name carries the bytes.
type ScorerDigest = [u8; 32];

/// The situs chain's content policies, read once per site render.
pub(crate) struct SitusPolicies {
    /// Most specific first, as `RegionRegistry::chain` returns them — the order
    /// the engine attributes a tie in.
    sets: Vec<RegionRuleSet>,
    /// The bundled scorers some rule in force reads, ready to run.
    scorers: Vec<(ScorerDigest, Arc<PreparedScorer>)>,
}

impl SitusPolicies {
    fn none() -> Self {
        Self {
            sets: Vec::new(),
            scorers: Vec::new(),
        }
    }

    /// Whether no region rule is in force — no situs, no cached document, or
    /// only documents that do not apply. A render skips the fold entirely then,
    /// which is every deployment that declares no region.
    pub(crate) fn is_inert(&self) -> bool {
        self.sets.iter().all(|set| set.rules.is_empty())
    }
}

/// Read the content policies in force for the nest's declared situs.
///
/// Each region on the chain answers from the relay's `(region, content-policy)`
/// cell — the one the region tier's worker refills, never a fetch of its own —
/// re-verified against `registry`: the cell only ever held what verified when it
/// was stored, and re-verifying here is what keeps an authority de-listed since
/// from binding a page in the window before the relay retires its document. A
/// region whose document is missing, no longer verifies or does not decode
/// contributes nothing and is logged; one whose document does not apply
/// contributes its (empty) rule set, which the engine ignores.
pub(crate) async fn situs_policies(
    db: &CacheDb,
    registry: &RegionRegistry,
) -> Result<SitusPolicies> {
    let Some(situs) = db.get_declared_region().await? else {
        return Ok(SitusPolicies::none());
    };
    let chain = match registry.chain(&situs) {
        Ok(chain) => chain,
        // A cycle or an over-deep chain is a broken registry, and the chain walk
        // refuses rather than applying whichever prefix it reached first.
        Err(e) => {
            tracing::error!(%situs, "region registry chain unusable, no region policy applied: {e}");
            return Ok(SitusPolicies::none());
        }
    };
    let now = crate::db::now_epoch_secs().max(0) as u64;
    let mut sets = Vec::with_capacity(chain.len());
    let mut wanted: Vec<(fauna_core::region_authority::RegionCode, BundledScorer)> = Vec::new();
    for region in chain {
        let Some(envelope) = db
            .relay_artifact(&region, PAYLOAD_KIND_CONTENT_POLICY)
            .await?
            .and_then(|cached| cached.envelope)
        else {
            continue;
        };
        let verified = match fauna_protocol::decode_strict::<PolicyArtifact>(&envelope)
            .map_err(anyhow::Error::from)
            .and_then(|artifact| {
                verify_artifact(artifact, registry, now, None).map_err(anyhow::Error::from)
            }) {
            Ok(verified) => verified,
            Err(e) => {
                tracing::warn!(%region, "cached content policy no longer verifies, not applied: {e:#}");
                continue;
            }
        };
        if verified.region() != &region {
            tracing::warn!(%region, "the content-policy cell holds another region's artifact, not applied");
            continue;
        }
        let document = match verified.content_policy() {
            Ok(document) => document,
            Err(e) => {
                tracing::warn!(%region, "cached content policy does not decode, not applied: {e}");
                continue;
            }
        };
        let set = rules_from_region_policy(&document, &region, verified.authority_name());
        if !set.status.is_applied() {
            tracing::warn!(%region, status = ?set.status, "region content policy is in force but does not apply");
        }
        wanted.extend(
            scorers_in_use(&document, &region)
                .into_iter()
                .map(|scorer| (region.clone(), scorer.clone())),
        );
        sets.push(set);
    }
    let scorers = if wanted.is_empty() {
        Vec::new()
    } else {
        // Compiling a module is CPU work — off the async workers.
        tokio::task::spawn_blocking(move || {
            wanted
                .into_iter()
                .filter_map(|(region, scorer)| {
                    match PreparedScorer::prepare(&region, &scorer) {
                        Ok(prepared) => Some((
                            *blake3::hash(&scorer.bytes).as_bytes(),
                            Arc::new(prepared),
                        )),
                        Err(e) => {
                            tracing::warn!(%region, scorer = %scorer.name, "bundled scorer unusable, its factor stays absent: {e:#}");
                            None
                        }
                    }
                })
                .collect()
        })
        .await?
    };
    Ok(SitusPolicies { sets, scorers })
}

/// Bundled-scorer answers, per actor, keyed by (post, scorer digest).
///
/// Each render of an actor's site takes that actor's answers out, re-runs only
/// what it has no answer for, and puts back exactly the answers it used — so
/// the cache never outgrows one site's posts × the chain's scorers, and a new
/// post or a new scorer is the only thing a render ever pays to score.
#[derive(Default)]
pub(crate) struct ScoreCache {
    by_actor: Mutex<HashMap<[u8; 32], HashMap<([u8; 32], ScorerDigest), Option<u16>>>>,
}

/// One render's view of an actor's cached answers.
pub(crate) struct ScoreRound {
    previous: HashMap<([u8; 32], ScorerDigest), Option<u16>>,
    used: HashMap<([u8; 32], ScorerDigest), Option<u16>>,
}

impl ScoreCache {
    pub(crate) fn begin(&self, actor: &[u8; 32]) -> ScoreRound {
        let previous = self
            .by_actor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(actor)
            .unwrap_or_default();
        ScoreRound {
            previous,
            used: HashMap::new(),
        }
    }

    pub(crate) fn finish(&self, actor: &[u8; 32], round: ScoreRound) {
        if round.used.is_empty() {
            return;
        }
        self.by_actor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(*actor, round.used);
    }
}

/// The labels the fold reads for one post: the ones the nest holds, plus each
/// in-use bundled scorer's factor over the post's public plaintext.
///
/// A scorer that fails (a trap, exhausted fuel or memory) answers nothing for
/// the post, and that answer is cached like any other: the module is
/// deterministic and would fail the same way on the next render.
pub(crate) async fn post_labels(
    db: &CacheDb,
    policies: &SitusPolicies,
    round: &mut ScoreRound,
    post_id: &[u8; 32],
    post: &fauna_core::data::Post,
) -> Result<Vec<ContentLabelEntry>> {
    let mut labels = db.post_label_entries(post_id).await?;
    if policies.scorers.is_empty() {
        return Ok(labels);
    }
    let input = Arc::new(LabelerPostInput::from_post(post));
    for (digest, scorer) in &policies.scorers {
        let key = (*post_id, *digest);
        let answer = match round.previous.get(&key) {
            Some(answer) => *answer,
            None => {
                let (scorer, input, id) = (Arc::clone(scorer), Arc::clone(&input), *post_id);
                let name = scorer.name.clone();
                match tokio::task::spawn_blocking(move || scorer.score(&id, &input)).await {
                    Ok(Ok(answer)) => answer,
                    Ok(Err(e)) => {
                        tracing::warn!(scorer = %name, "bundled scorer failed on a post, its factor stays absent: {e:#}");
                        None
                    }
                    Err(e) => {
                        tracing::warn!(scorer = %name, "bundled scorer task failed: {e}");
                        None
                    }
                }
            }
        };
        round.used.insert(key, answer);
        if let Some(confidence_per_mille) = answer {
            labels.push(ContentLabelEntry {
                category: scorer.factor.clone(),
                confidence_per_mille,
            });
        }
    }
    Ok(labels)
}

/// What a region verdict does to one post's public page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageVerdict {
    /// The page shows the reasoned notice in place of the post.
    Block,
    /// The page shows the notice with the post behind a reveal.
    Collapse,
}

/// A region verdict on one post, with the attribution its placeholder shows.
#[derive(Debug, Clone)]
pub(crate) struct RegionPlacement {
    pub(crate) verdict: PageVerdict,
    attribution: RegionAttribution,
}

/// Fold one post's labels through the shared engine with the region source
/// only. `None` unless a region rule blocks or collapses it — a badge has no
/// placeholder, and with no other source in the call a block or collapse is
/// always the region's.
pub(crate) fn placement(
    policies: &SitusPolicies,
    labels: &[ContentLabelEntry],
) -> Option<RegionPlacement> {
    let composed = render_verdict_composed(labels, None, None, &policies.sets);
    let verdict = match composed.verdict {
        RenderVerdict::Block => PageVerdict::Block,
        RenderVerdict::Collapse => PageVerdict::Collapse,
        RenderVerdict::Show | RenderVerdict::Badge => return None,
    };
    Some(RegionPlacement {
        verdict,
        attribution: composed.region()?.clone(),
    })
}

impl RegionPlacement {
    /// Whether the post's own body must not reach any page — a block. The
    /// caller also never decrypts a blocked gated post's full body: the sealed
    /// full page is the same post published by the same nest, so it is withheld
    /// with the public one rather than left to serve what the public page hides.
    pub(crate) fn withholds_body(&self) -> bool {
        self.verdict == PageVerdict::Block
    }

    /// Rewrite a post's page context into the reasoned placeholder
    /// (`region-blocking.md` § The blocked render): that it was blocked or
    /// collapsed, under which region's authority, and the authority's reason
    /// verbatim — the frame in the words every app uses, the reason never
    /// paraphrased.
    ///
    /// The title and tags go too, under either verdict: both are read off the
    /// body, and they are what the index, the tag pages and the feed list. A
    /// block also drops the paywall box — there is nothing on the page to
    /// subscribe to. The markup carries neutral classes and data attributes,
    /// not test ids: the page's element ids await their own approval.
    pub(crate) fn apply(&self, ctx: &mut PostContext) {
        let region = self.attribution.region.as_str();
        let authority = self.attribution.authority_name.as_str();
        let reason = self
            .attribution
            .reason
            .get(REASON_DEFAULT_KEY)
            .map(String::as_str)
            .unwrap_or_default();
        let escape = |text: &str| fauna_core::markdown::escape_html_with(text, true);
        let attributes = format!(
            "data-region=\"{}\" data-reason-code=\"{}\"",
            escape(region),
            escape(&self.attribution.reason_code)
        );
        match self.verdict {
            PageVerdict::Block => {
                let frame = fauna_i18n::strings::region::blocked_notice(region, authority);
                ctx.content = format!(
                    "<div class=\"fauna-region-notice\" data-region-verdict=\"block\" {attributes}>\
                     <p class=\"fauna-region-notice-frame\">{}</p>\
                     <p class=\"fauna-region-notice-reason\">{}</p>\
                     </div>",
                    escape(&frame),
                    escape(reason),
                );
                ctx.title = frame;
                ctx.paywall = None;
            }
            PageVerdict::Collapse => {
                let frame = fauna_i18n::strings::region::collapsed_notice(region, authority);
                ctx.content = format!(
                    "<details class=\"fauna-region-notice\" data-region-verdict=\"collapse\" {attributes}>\
                     <summary class=\"fauna-region-notice-frame\">{}</summary>\
                     <p class=\"fauna-region-notice-reason\">{}</p>\
                     <div class=\"fauna-region-collapsed-content\">{}</div>\
                     </details>",
                    escape(&frame),
                    escape(reason),
                    ctx.content,
                );
                ctx.title = frame;
            }
        }
        ctx.tags.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> PostContext {
        PostContext {
            title: "A title read off the body".into(),
            slug: "the-slug".into(),
            content: "<p>the body</p>".into(),
            created_at: 1,
            tags: vec!["fromthebody".into()],
            next: Some("newer".into()),
            prev: None,
            paywall: Some(super::super::render::PaywallContext {
                tier: "gold".into(),
                price_hint: None,
                payment_url: None,
            }),
        }
    }

    fn placement(verdict: PageVerdict) -> RegionPlacement {
        RegionPlacement {
            verdict,
            attribution: RegionAttribution {
                region: fauna_core::region_authority::RegionCode::parse("NO").unwrap(),
                authority_name: "Fixture \"Authority\"".into(),
                reason_code: "FX-7".into(),
                reason: std::collections::BTreeMap::from([(
                    REASON_DEFAULT_KEY.to_string(),
                    "<b>not</b> markup".to_string(),
                )]),
            },
        }
    }

    /// Everything read off the body leaves the page under a block — title,
    /// body and tags — and the slug and neighbours (the author's URL and the
    /// site's order) stay; every piece of authority text is escaped.
    #[test]
    fn a_block_replaces_everything_read_off_the_body() {
        let mut ctx = context();
        placement(PageVerdict::Block).apply(&mut ctx);
        assert_eq!(
            ctx.title,
            "Not shown in NO — blocked under the policy of Fixture \"Authority\""
        );
        assert!(!ctx.content.contains("the body"), "{}", ctx.content);
        assert!(
            ctx.content.contains("Fixture &quot;Authority&quot;"),
            "{}",
            ctx.content
        );
        assert!(
            ctx.content.contains("&lt;b&gt;not&lt;/b&gt; markup"),
            "{}",
            ctx.content
        );
        assert!(ctx.tags.is_empty());
        assert!(ctx.paywall.is_none());
        assert_eq!(ctx.slug, "the-slug");
        assert_eq!(ctx.next.as_deref(), Some("newer"));
    }

    /// A collapse keeps the body — behind the reveal — and the paywall box,
    /// but lists neither title nor tags read off the body.
    #[test]
    fn a_collapse_keeps_the_body_behind_the_reveal() {
        let mut ctx = context();
        placement(PageVerdict::Collapse).apply(&mut ctx);
        assert!(ctx.title.starts_with("Hidden in NO"), "{}", ctx.title);
        assert!(ctx.content.starts_with("<details"), "{}", ctx.content);
        assert!(ctx.content.contains("<p>the body</p>"), "{}", ctx.content);
        assert!(ctx.tags.is_empty());
        assert!(ctx.paywall.is_some());
    }

    /// The fold is called from exactly one nest module — this one. No other
    /// nest surface evaluates content (`region-blocking.md` invariant 4), and a
    /// second call site would be exactly such a surface.
    #[test]
    fn the_fold_has_exactly_one_caller() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut callers = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    if text.contains(concat!("render_verdict", "_composed(")) {
                        // `/`-joined, so the answer reads the same on every OS.
                        let relative: Vec<String> = path
                            .strip_prefix(&src)
                            .unwrap()
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect();
                        callers.push(relative.join("/"));
                    }
                }
            }
        }
        assert_eq!(
            callers,
            ["web_content/region.rs"],
            "the region fold must stay the public web render's alone"
        );
    }
}
