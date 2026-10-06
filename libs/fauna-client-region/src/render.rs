//! What a region verdict paints — the blocked render (`region-blocking.md`
//! § The blocked render and the transparency surface), folded once here so no
//! shell decides for itself which verbs get a placeholder or which language of
//! the authority's reason it shows.
//!
//! A shell asks [`placeholder_for`] with the composed verdict it already holds
//! and paints the answer **in place of** the item: its own frame
//! (`region.blocked_notice` / `region.collapsed_notice`) naming [`RegionPlaceholder::region`]
//! and [`RegionPlaceholder::authority_name`], the authority's name, and
//! [`RegionPlaceholder::reason`] verbatim — never an i18n string; a
//! [`RegionVerb::Collapse`] adds the reveal.

use fauna_core::obligation::{ComposedVerdict, RenderVerdict};
use fauna_core::region_authority::RegionCode;
use fauna_core::region_policy::REASON_DEFAULT_KEY;

/// The two verbs a region placeholder paints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionVerb {
    /// Withheld; no reveal.
    Block,
    /// Hidden behind the reveal.
    Collapse,
}

impl RegionVerb {
    /// `block` | `collapse` — the value the placeholder's `verdict` attribute
    /// carries, which the convention-17 `region-block-never-silent` invariant
    /// counts block placeholders by.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Collapse => "collapse",
        }
    }
}

/// One region placeholder, ready to paint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionPlaceholder {
    pub verb: RegionVerb,
    pub region: RegionCode,
    /// As the registry names the authority.
    pub authority_name: String,
    /// The authority's own words in the app's language, else its `default`
    /// text — shown verbatim.
    pub reason: String,
}

/// The placeholder a composed verdict paints, when the **region** drove a
/// `block` or `collapse` — `None` for every other verdict (a family-floor or
/// own-threshold verdict keeps its own placeholder, which the app names in its
/// own words). `lang` is the app's UI language.
pub fn placeholder_for(verdict: &ComposedVerdict, lang: &str) -> Option<RegionPlaceholder> {
    let attribution = verdict.region()?;
    let verb = match verdict.verdict {
        RenderVerdict::Block => RegionVerb::Block,
        RenderVerdict::Collapse => RegionVerb::Collapse,
        RenderVerdict::Show | RenderVerdict::Badge => return None,
    };
    let reason = attribution
        .reason
        .get(lang)
        .or_else(|| attribution.reason.get(REASON_DEFAULT_KEY))
        .cloned()
        .unwrap_or_default();
    Some(RegionPlaceholder {
        verb,
        region: attribution.region.clone(),
        authority_name: attribution.authority_name.clone(),
        reason,
    })
}
