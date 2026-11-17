//! `Possibility` — the one answer every gated affordance's predicate gives
//! (`docs/goal/ui/README.md` § Offer only what is possible, rule 6).
//!
//! An app never decides on its own whether an affordance can succeed now.
//! Beside each gated action sits one shared-Rust predicate, and it answers with
//! this type: [`Possibility::Possible`], [`Possibility::Blocked`] with the
//! reason the person can act on (rule 2: disabled, the reason within eyeshot),
//! or [`Possibility::Absent`] when this person can never do it on this app
//! (rule 2: not shown at all). Every app paints from the answer — enabled,
//! disabled with the reason's string, or left out — and the action asks the
//! same predicate before it runs, so what the page offers and what the action
//! accepts cannot disagree.
//!
//! It lives in `fauna-core` beside [`LocalizedText`] because every predicate's
//! crate already depends on this one: the onboarding machine, provisioning,
//! moderation and the protocol's offline classification
//! (`fauna_protocol::offline_class::Affordance` converts into it) can all
//! answer in this one type without a new dependency edge. One registration of
//! the type also means one generated binding for every app family.

use serde::Serialize;

use crate::localized::LocalizedText;

/// Whether an affordance can succeed **now**, and if not, why.
///
/// The serde form is `{"state": "possible"}`, `{"state": "blocked", "reason":
/// {…}}` or `{"state": "absent"}`, the shape the web SPA reads. It is
/// serialize-only on purpose: an answer is computed where it is shown and
/// never decoded off a wire or out of storage, so it owes no forward-compat
/// catch-all (`transport.md` § Schema and forward-compat discipline, rule 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum Possibility {
    /// Offer it, enabled.
    Possible,
    /// Show it disabled, with `reason` painted on the same surface. The person
    /// can make the precondition true from this app (or it becomes true by
    /// waiting: the connection returning, a read resolving).
    Blocked { reason: LocalizedText },
    /// Do not show it. This person cannot make the precondition true on this
    /// app at all: a role or grant they lack, a platform absence, a feature
    /// the nest does not have.
    Absent,
}

/// Where the read that decides an affordance stands (`docs/goal/ui/README.md`
/// § Offer only what is possible, rule 5: unknown is not possible).
///
/// A page whose affordance waits on a read hands the read's state to
/// [`Possibility::from_read`], which gives the one answer every app paints
/// from: a pre-read control is never enabled on a guess, and never disabled
/// without its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ReadState {
    /// Not yet answered: what the read decides is not known.
    #[default]
    Pending,
    /// The read came back with an error: the answer is still not known, and
    /// the person is told the read failed rather than that it is loading.
    Failed,
    /// The read resolved: the page decides from what it holds.
    Resolved,
}

impl Possibility {
    /// The answer a read alone gives (rule 5): *blocked* with
    /// `common.still_loading` while it is pending, *blocked* with
    /// `common.load_failed` when it failed, *possible* once it resolved. A
    /// page whose affordance needs more than the read folds its own answer in
    /// with [`Self::and`].
    pub fn from_read(read: ReadState) -> Self {
        match read {
            ReadState::Pending => Possibility::blocked(LocalizedText::key("common.still_loading")),
            ReadState::Failed => Possibility::blocked(LocalizedText::key("common.load_failed")),
            ReadState::Resolved => Possibility::Possible,
        }
    }

    /// [`Self::from_read`] for a read a page tracks only as answered or not.
    pub fn once_read(resolved: bool) -> Self {
        Self::from_read(if resolved {
            ReadState::Resolved
        } else {
            ReadState::Pending
        })
    }

    /// A blocked answer for an i18n key with no substitutions.
    pub fn blocked(reason: LocalizedText) -> Self {
        Possibility::Blocked { reason }
    }

    /// Adapt the older `*_blocked_reason() -> Option<LocalizedText>` getter
    /// shape: `None` is possible, `Some(reason)` is blocked by it. This is the
    /// shape `providers_continue_blocked_reason()` and its siblings already
    /// return, so they move onto this type with no change of meaning.
    pub fn from_blocked_reason(reason: Option<LocalizedText>) -> Self {
        match reason {
            None => Possibility::Possible,
            Some(reason) => Possibility::Blocked { reason },
        }
    }

    /// `true` only for [`Possibility::Possible`], the boolean an app hands
    /// its widget's own sensitivity flag.
    pub fn is_possible(&self) -> bool {
        matches!(self, Possibility::Possible)
    }

    /// `false` only for [`Possibility::Absent`]: whether the affordance is
    /// painted at all.
    pub fn is_shown(&self) -> bool {
        !matches!(self, Possibility::Absent)
    }

    /// The reason to paint beside a disabled affordance. `None` when it is
    /// possible, and when it is absent (nothing is painted, so there is
    /// nothing to explain).
    pub fn reason(&self) -> Option<&LocalizedText> {
        match self {
            Possibility::Blocked { reason } => Some(reason),
            _ => None,
        }
    }

    /// Both preconditions at once, as when the element seam folds a page's
    /// own predicate with the offline gate. `Absent` wins (it can never be
    /// done here, whatever else holds). Next comes the first `Blocked`: the
    /// page's own reason is the more specific one, so callers pass it as
    /// `self`. Only two `Possible` answers give `Possible`.
    pub fn and(self, other: Possibility) -> Possibility {
        match (self, other) {
            (Possibility::Absent, _) | (_, Possibility::Absent) => Possibility::Absent,
            (blocked @ Possibility::Blocked { .. }, _) => blocked,
            (Possibility::Possible, other) => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(key: &str) -> Possibility {
        Possibility::blocked(LocalizedText::key(key))
    }

    #[test]
    fn the_getter_shape_maps_without_a_change_of_meaning() {
        assert_eq!(
            Possibility::from_blocked_reason(None),
            Possibility::Possible
        );
        assert_eq!(
            Possibility::from_blocked_reason(Some(LocalizedText::key("a"))),
            blocked("a")
        );
    }

    #[test]
    fn only_possible_is_enabled_and_only_absent_is_hidden() {
        assert!(Possibility::Possible.is_possible());
        assert!(!blocked("a").is_possible());
        assert!(!Possibility::Absent.is_possible());
        assert!(Possibility::Possible.is_shown());
        assert!(blocked("a").is_shown());
        assert!(!Possibility::Absent.is_shown());
    }

    #[test]
    fn a_reason_exists_exactly_when_blocked() {
        assert_eq!(Possibility::Possible.reason(), None);
        assert_eq!(Possibility::Absent.reason(), None);
        assert_eq!(blocked("a").reason().map(|r| r.key.as_str()), Some("a"));
    }

    #[test]
    fn and_lets_absent_win_then_the_first_reason() {
        use Possibility::{Absent, Possible};
        assert_eq!(Possible.and(Possible), Possible);
        assert_eq!(Possible.and(blocked("b")), blocked("b"));
        assert_eq!(blocked("a").and(Possible), blocked("a"));
        assert_eq!(blocked("a").and(blocked("b")), blocked("a"));
        assert_eq!(blocked("a").and(Absent), Absent);
        assert_eq!(Absent.and(Possible), Absent);
    }

    #[test]
    fn an_unresolved_read_is_never_possible_and_always_says_why() {
        assert_eq!(
            Possibility::from_read(ReadState::Pending),
            blocked("common.still_loading")
        );
        assert_eq!(
            Possibility::from_read(ReadState::Failed),
            blocked("common.load_failed")
        );
        assert_eq!(
            Possibility::from_read(ReadState::Resolved),
            Possibility::Possible
        );
        assert_eq!(
            Possibility::once_read(false),
            blocked("common.still_loading")
        );
        assert_eq!(Possibility::once_read(true), Possibility::Possible);
    }

    #[test]
    fn the_serde_form_is_the_tagged_shape_web_reads() {
        assert_eq!(
            serde_json::to_value(Possibility::Possible).unwrap(),
            serde_json::json!({"state": "possible"})
        );
        assert_eq!(
            serde_json::to_value(blocked("k")).unwrap(),
            serde_json::json!({"state": "blocked", "reason": {"key": "k", "args": {}}})
        );
        assert_eq!(
            serde_json::to_value(Possibility::Absent).unwrap(),
            serde_json::json!({"state": "absent"})
        );
    }
}
