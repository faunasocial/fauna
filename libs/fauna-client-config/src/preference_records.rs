//! Preference mutation logic defined on the **sub-record** — the
//! storage-agnostic half of every preference cluster.
//!
//! # Why this module exists
//!
//! A preference cluster lives on the account plane
//! (`docs/goal/architecture/config-dissolution.md` § The `__config`
//! dissolution schedule, the E1 cluster): a plane entry's value is *exactly*
//! the cluster's sub-record in canonical dag-cbor — `moderation`,
//! `sync_prefs`, `personalization`, `delegation` (the admitted table in
//! `fauna_sync_engine::preference_put`).
//!
//! The meaning of each gesture is here — one function per gesture, taking
//! `&mut SubRecord` — and the plane's surfaces
//! (`fauna_sync_engine::preference_surfaces`) call these directly.
//!
//! The one rule for anything added here: **no clock, no I/O** — a pure
//! function of the sub-record it is handed. Anything that needs a timestamp
//! belongs in [`crate::mutate`].

use fauna_core::data::{
    ModerationConfig, MutedKeyword, PersonalizationConfig, SyncPrefsConfig, Timestamp,
    TrainedFactorMeta,
};
use fauna_protocol::personalization::TRAINED_FACTORS_MAX;

use crate::mutate::normalize_muted_keywords;

// ── moderation ──

/// Replace the muted-keywords list with its normalized form
/// ([`normalize_muted_keywords`]).
pub fn set_muted_keywords(moderation: &mut ModerationConfig, keywords: Vec<MutedKeyword>) {
    moderation.muted_keywords = normalize_muted_keywords(keywords);
}

/// Add one term to the muted-keywords list — the **delta** form of
/// [`set_muted_keywords`], applied against whatever the record holds *at apply
/// time* rather than against a page's snapshot.
///
/// This distinction is the whole point: every app used to send
/// its in-memory list wholesale, so a term another device stored between this
/// page's load and this click was silently clobbered — a merge cannot know one
/// *list replacement* meant "add one term". Expressed as a delta over the
/// current record, it is applied inside the plane's read-modify-write
/// (`update_record`), so a retry re-applies against the then-current list and
/// the concurrent term survives.
///
/// Normalization is [`set_muted_keywords`]'s, unchanged: appending keeps the
/// first-seen entry on a case-insensitive duplicate, so re-adding an existing
/// term is a no-op rather than an error — and never resets a weight the user
/// gave it. A new term is muted at the default weight, the full penalty.
pub fn add_muted_keyword(moderation: &mut ModerationConfig, word: &str) {
    let mut list = moderation.muted_keywords.clone();
    list.push(MutedKeyword::new(word));
    set_muted_keywords(moderation, list);
}

/// Set one muted term's weight — the delta behind a per-keyword weight
/// control (`content-moderation-and-ranking.md` § Composition, the 2026-07-10
/// ruling). Matches the stored spelling exactly, like
/// [`remove_muted_keyword`]; a term the list does not hold is a no-op. The
/// weight is clamped into `[MUTED_KEYWORDS_PENALTY, 0]` by the shared
/// normalizer.
pub fn set_muted_keyword_weight(moderation: &mut ModerationConfig, word: &str, weight: i64) {
    let list: Vec<MutedKeyword> = moderation
        .muted_keywords
        .iter()
        .map(|k| {
            if k.keyword == word {
                MutedKeyword {
                    keyword: k.keyword.clone(),
                    weight,
                }
            } else {
                k.clone()
            }
        })
        .collect();
    set_muted_keywords(moderation, list);
}

/// Remove one term from the muted-keywords list — [`add_muted_keyword`]'s
/// inverse, same delta-over-current-record reasoning.
///
/// Matches the **stored spelling exactly**: the rows a remove button sits on
/// are always normalized stored spellings (every read and save returns them),
/// so an exact match is total over what a UI can ask, and a term the list does
/// not hold is a no-op — removing something already gone is success, not an
/// error.
pub fn remove_muted_keyword(moderation: &mut ModerationConfig, word: &str) {
    let list: Vec<MutedKeyword> = moderation
        .muted_keywords
        .iter()
        .filter(|k| k.keyword != word)
        .cloned()
        .collect();
    set_muted_keywords(moderation, list);
}

/// The most report subjects `hidden_content` keeps. A report is rate-capped at
/// `ABUSE_REPORTS_PER_HOUR`, so this is years of reporting — and it keeps the
/// `fauna.state.moderation` entry far under its per-entry cap. Past it, the OLDEST
/// entry leaves first: the newest reports are the ones a user is still
/// scrolling past.
pub const MAX_HIDDEN_CONTENT: usize = 2_000;

/// Normalize one report subject id for `hidden_content`: trimmed and
/// lowercased, so a case-varied hex id is the same entry. `None` for a blank.
fn normalize_hidden_id(id: &str) -> Option<String> {
    let id = id.trim().to_ascii_lowercase();
    (!id.is_empty()).then_some(id)
}

/// Hide something the user just reported, for them (`moderation.md` §
/// Corollary — block also hides): append the subject's id to
/// `hidden_content`. A delta over the record as the update reads it — the
/// [`add_muted_keyword`] reasoning — so an entry another device stored
/// meanwhile survives. Re-hiding an id already there is a no-op; past
/// [`MAX_HIDDEN_CONTENT`] the oldest entry leaves.
pub fn hide_reported_content(moderation: &mut ModerationConfig, id: &str) {
    let Some(id) = normalize_hidden_id(id) else {
        return;
    };
    if moderation.hidden_content.contains(&id) {
        return;
    }
    moderation.hidden_content.push(id);
    let excess = moderation
        .hidden_content
        .len()
        .saturating_sub(MAX_HIDDEN_CONTENT);
    moderation.hidden_content.drain(..excess);
}

/// Show again something the user hid by reporting it — the inverse of
/// [`hide_reported_content`], same delta reasoning. Unknown ids are a no-op.
pub fn unhide_reported_content(moderation: &mut ModerationConfig, id: &str) {
    if let Some(id) = normalize_hidden_id(id) {
        moderation.hidden_content.retain(|h| *h != id);
    }
}

// ── sync prefs ──

/// Set (or clear, with `None`) the default conflict policy stamped onto newly
/// created folders, normalized to a canonical wire string — an unknown value
/// degrades to `"auto"` rather than being stored verbatim, so a select can only
/// ever show a real option.
pub fn set_default_conflict_policy(prefs: &mut SyncPrefsConfig, policy: Option<&str>) {
    prefs.default_conflict_policy = policy.map(|p| {
        fauna_core::format::ConflictPolicy::from_wire(p)
            .as_str()
            .to_string()
    });
}

// ── personalization: the trained-topic registry ──

/// Mint a trained topic factor (fresh 16-byte random id + epoch-seconds
/// creation stamp, `learn_from_engagement` off per v1) and append it, returning
/// the new entry.
///
/// `None` (a no-op) when the trimmed name is empty, or the registry is already
/// at [`TRAINED_FACTORS_MAX`].
pub fn add_trained_factor(
    personalization: &mut PersonalizationConfig,
    name: &str,
) -> Option<TrainedFactorMeta> {
    let name = name.trim();
    if name.is_empty() || personalization.trained_factors.len() >= TRAINED_FACTORS_MAX {
        return None;
    }
    let mut id = vec![0u8; 16];
    getrandom::fill(&mut id).expect("getrandom failed");
    let meta = TrainedFactorMeta {
        id,
        name: name.to_string(),
        learn_from_engagement: false,
        created_at: u64::try_from(Timestamp::now_secs()).unwrap_or(0),
    };
    personalization.trained_factors.push(meta.clone());
    Some(meta)
}

/// Rename the factor with `id` (trimmed). `false` on an unknown id or a blank
/// name (both no-ops). The id — and so the derived composition key — never
/// changes: every composition referencing the factor keeps working.
pub fn rename_trained_factor(
    personalization: &mut PersonalizationConfig,
    id: &[u8],
    name: &str,
) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    let Some(entry) = personalization
        .trained_factors
        .iter_mut()
        .find(|f| f.id == id)
    else {
        return false;
    };
    entry.name = name.to_string();
    true
}

/// Set the factor's `learn_from_engagement` opt-in. `false` on an unknown id (a
/// no-op — the row was deleted on another device between render and commit).
pub fn set_learn_from_engagement(
    personalization: &mut PersonalizationConfig,
    id: &[u8],
    on: bool,
) -> bool {
    let Some(entry) = personalization
        .trained_factors
        .iter_mut()
        .find(|f| f.id == id)
    else {
        return false;
    };
    entry.learn_from_engagement = on;
    true
}

/// Remove the factor with `id` and return the **removed entry**; `None` when no
/// entry matched (a no-op).
///
/// The entry rather than its derived key, deliberately: a matched entry with a
/// corrupt id has no derivable key, so a `String` return could not tell "nothing
/// matched" from "removed, but nothing is addressable nest-side" — and the two
/// differ, since only the first leaves the record unchanged. Callers pair
/// [`TrainedFactorMeta::factor_key`] on the returned entry with
/// `fauna.personalization.model.delete` (`topic-factors.md` § Delete
/// semantics).
pub fn remove_trained_factor(
    personalization: &mut PersonalizationConfig,
    id: &[u8],
) -> Option<TrainedFactorMeta> {
    let idx = personalization
        .trained_factors
        .iter()
        .position(|f| f.id == id)?;
    Some(personalization.trained_factors.remove(idx))
}

// ── delegation ──
//
// The fourth cluster needs nothing here, and its absence is deliberate rather
// than an oversight: the pin mutation is already sub-record-level
// (`fauna_core::data::DelegationConfig::set_pin`, a method on the sub-record
// itself), guarded by the equally shared `fauna_core::delegation::resolve_pin`.
// The plane's surfaces call those two directly.

#[cfg(test)]
mod tests {
    use super::*;

    /// The sub-record mutator normalizes the way the plane's stored record
    /// expects — the property byte-identical stores depend on.
    #[test]
    fn conflict_policy_normalizes_and_clears() {
        let mut prefs = SyncPrefsConfig::default();
        assert_eq!(prefs.default_conflict_policy, None);

        set_default_conflict_policy(&mut prefs, Some("latest_wins_always"));
        assert_eq!(
            prefs.default_conflict_policy.as_deref(),
            Some("latest_wins_always")
        );

        // Out-of-catalog degrades to the canonical default, never stored raw.
        set_default_conflict_policy(&mut prefs, Some("frobnicate"));
        assert_eq!(prefs.default_conflict_policy.as_deref(), Some("auto"));

        set_default_conflict_policy(&mut prefs, None);
        assert_eq!(prefs.default_conflict_policy, None);
    }

    #[test]
    fn muted_keywords_normalize_on_the_sub_record() {
        let mut moderation = ModerationConfig::default();
        set_muted_keywords(
            &mut moderation,
            vec![" Cats ".into(), "cats".into(), "".into(), "Dogs".into()],
        );
        assert_eq!(
            moderation.muted_keywords,
            vec![MutedKeyword::from("Cats"), MutedKeyword::from("Dogs")]
        );
    }

    /// The weight delta: set on the stored spelling, clamped, a no-op on a
    /// term the list does not hold — and a re-add keeps the weight.
    #[test]
    fn a_muted_keyword_weight_is_set_clamped_and_survives_a_re_add() {
        let mut moderation = ModerationConfig::default();
        add_muted_keyword(&mut moderation, "Politics");
        assert_eq!(
            moderation.muted_keywords[0].weight,
            fauna_core::scoring::MUTED_KEYWORDS_PENALTY,
            "a new term mutes at the full penalty"
        );
        set_muted_keyword_weight(&mut moderation, "Politics", -250);
        assert_eq!(moderation.muted_keywords[0].weight, -250);
        add_muted_keyword(&mut moderation, "politics");
        assert_eq!(
            moderation.muted_keywords,
            vec![MutedKeyword {
                keyword: "Politics".into(),
                weight: -250
            }],
            "re-adding the term keeps the entry and its weight"
        );
        set_muted_keyword_weight(&mut moderation, "Politics", 900);
        assert_eq!(moderation.muted_keywords[0].weight, 0, "clamped");
        set_muted_keyword_weight(&mut moderation, "nope", -1);
        assert_eq!(moderation.muted_keywords.len(), 1, "unknown term: no-op");
    }

    /// Create → rename → flag → remove, all against the bare registry: the cap
    /// and the unknown-id no-ops hold without any plane entry in sight.
    #[test]
    fn the_trained_factor_registry_round_trips_on_the_sub_record() {
        let mut p = PersonalizationConfig::default();

        assert!(add_trained_factor(&mut p, "   ").is_none(), "blank name");
        let meta = add_trained_factor(&mut p, " Cats ").expect("create");
        assert_eq!(meta.name, "Cats", "trimmed");
        assert_eq!(p.trained_factors.len(), 1);

        assert!(rename_trained_factor(&mut p, &meta.id, " Felines "));
        assert_eq!(p.trained_factors[0].name, "Felines");
        assert!(!rename_trained_factor(&mut p, &meta.id, "  "), "blank");
        assert!(!rename_trained_factor(&mut p, b"nope", "x"), "unknown id");

        assert!(set_learn_from_engagement(&mut p, &meta.id, true));
        assert!(p.trained_factors[0].learn_from_engagement);
        assert!(!set_learn_from_engagement(&mut p, b"nope", true));

        let removed = remove_trained_factor(&mut p, &meta.id).expect("removed");
        assert_eq!(removed.factor_key(), meta.factor_key());
        assert!(p.trained_factors.is_empty());
        assert!(remove_trained_factor(&mut p, &meta.id).is_none(), "gone");
    }

    /// The cap is the registry's own, not the whole record's.
    #[test]
    fn the_create_cap_is_enforced_on_the_registry_alone() {
        let mut p = PersonalizationConfig::default();
        for i in 0..TRAINED_FACTORS_MAX {
            assert!(
                add_trained_factor(&mut p, &format!("t{i}")).is_some(),
                "factor {i} fits"
            );
        }
        assert!(add_trained_factor(&mut p, "one too many").is_none());
        assert_eq!(p.trained_factors.len(), TRAINED_FACTORS_MAX);
    }

    /// A reported subject is hidden once, case-insensitively, beside the
    /// muted words it shares a record with; unhiding removes it; past the cap
    /// the oldest entry leaves first.
    #[test]
    fn hidden_content_is_a_bounded_case_insensitive_delta_pair() {
        let mut m = ModerationConfig {
            muted_keywords: vec!["spoiler".into()],
            ..Default::default()
        };
        hide_reported_content(&mut m, " ABCD ");
        hide_reported_content(&mut m, "abcd");
        hide_reported_content(&mut m, "  ");
        assert_eq!(m.hidden_content, vec!["abcd".to_string()]);
        assert_eq!(m.muted_keywords, vec![MutedKeyword::from("spoiler")]);
        unhide_reported_content(&mut m, "ABCD");
        unhide_reported_content(&mut m, "never-hidden");
        assert!(m.hidden_content.is_empty());

        for i in 0..=MAX_HIDDEN_CONTENT {
            hide_reported_content(&mut m, &format!("{i:x}"));
        }
        assert_eq!(m.hidden_content.len(), MAX_HIDDEN_CONTENT);
        assert_eq!(m.hidden_content[0], "1", "the oldest left first");
    }
}
