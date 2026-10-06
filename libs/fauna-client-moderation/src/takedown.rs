//! The legal-takedown console's shared decision logic (`moderation.md` § Legal
//! takedown → *Invocation surface*, ruled 2026-08-16) — form gating, the confirm
//! summary, and the verdict wording, derived once here rather than seven times
//! (priority #2).
//!
//! Four decisions live here because each is one an app would get wrong alone:
//!
//! 1. **A citation-less takedown is never armable.** The wire refuses a takedown
//!    with an empty `legal_reference` (the structural guard that makes this
//!    compulsion, not policy); rendering that guard client-side means the arm
//!    control is disabled *with a stated reason* instead of a dispatch that
//!    bounces off `invalid_params`.
//! 2. **The guard's asymmetry.** On `restore` the reference is the *optional*
//!    overturn note — a restore with no note must stay armable. A per-app
//!    "require the reference" check would silently break the overturn.
//! 3. **The confirm names what it does.** The armed summary carries the verb,
//!    the content id, and the citation — a compulsory act is never confirmed
//!    blind. Every app renders the same summary or the transparency framing
//!    drifts.
//! 4. **The verdict wording.** Success is not `error-message` material, and the
//!    two verbs have different consequences worth different sentences (a
//!    restore keeps the takedown row as additive history — say so).

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

/// The content kind under the legal obligation. Wire values:
/// [`TakedownContentType::wire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TakedownContentType {
    /// A social post (`content_meta.legal_takedown_ref` — the serve-withhold half).
    #[default]
    Post,
    /// An MLS conversation message (`segment_records.legal_takedown_ref` — the
    /// relay-withhold half, keyed on the message's record cid).
    Conversation,
}

impl TakedownContentType {
    /// The `fauna.moderation.legal_takedown` `content_type` string.
    pub fn wire(self) -> &'static str {
        match self {
            TakedownContentType::Post => "post",
            TakedownContentType::Conversation => "conversation",
        }
    }
}

/// The console's draft state, exactly as typed — apps pass their raw field
/// values and render whatever comes back; nothing here is trimmed in place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakedownForm {
    /// The content id draft (post: 32-byte hex; conversation: the record cid).
    /// Only presence is checked here — id *shape* is the nest's to judge
    /// (`invalid_params` surfaces on the app's error line), so seven apps don't
    /// grow seven divergent id validators.
    pub content_id: String,
    pub content_type: TakedownContentType,
    /// The legal-obligation reference draft (required for a takedown; the
    /// optional overturn note on restore).
    pub legal_reference: String,
    /// `false` = take down; `true` = overturn/restore.
    pub restore: bool,
}

/// What the console renders, derived once per keystroke from the form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakedownFormView {
    /// Whether the arm control may be offered.
    pub can_submit: bool,
    /// Why not, when it may not — the page states the reason instead of
    /// painting a dead control.
    pub blocked_reason: Option<LocalizedText>,
    /// The arm control's own label — the verb flips with `restore`.
    pub arm_label: LocalizedText,
    /// The armed confirm's decision surface: names the verb, the content, and
    /// (for a takedown) the citation the dispatch will record.
    pub confirm_summary: LocalizedText,
    /// The confirm control's label (verb-matched).
    pub confirm_label: LocalizedText,
}

/// Fold the form into the console's render decisions.
pub fn takedown_form_view(form: &TakedownForm) -> TakedownFormView {
    let content_id = form.content_id.trim();
    let reference = form.legal_reference.trim();

    let blocked_reason = if content_id.is_empty() {
        Some(LocalizedText::key(
            "admin.nest_page.takedown_blocked_no_content",
        ))
    } else if !form.restore && reference.is_empty() {
        // The wire's own structural guard, rendered (decision 1); deliberately
        // NOT applied on restore (decision 2).
        Some(LocalizedText::key(
            "admin.nest_page.takedown_blocked_no_reference",
        ))
    } else {
        None
    };

    let (arm_key, summary_key, confirm_key) = if form.restore {
        (
            "admin.nest_page.takedown_arm_restore",
            "admin.nest_page.takedown_confirm_restore",
            "admin.nest_page.takedown_confirm_button_restore",
        )
    } else {
        (
            "admin.nest_page.takedown_arm_takedown",
            "admin.nest_page.takedown_confirm_takedown",
            "admin.nest_page.takedown_confirm_button_takedown",
        )
    };

    let mut summary_args = vec![
        ("content_type", form.content_type.wire().to_string()),
        ("content_id", content_id.to_string()),
    ];
    if !form.restore {
        summary_args.push(("reference", reference.to_string()));
    }

    TakedownFormView {
        can_submit: blocked_reason.is_none(),
        blocked_reason,
        arm_label: LocalizedText::key(arm_key),
        confirm_summary: LocalizedText::key_args(summary_key, summary_args),
        confirm_label: LocalizedText::key(confirm_key),
    }
}

/// The outcome line the console paints after a dispatch — deliberately not
/// `error-message` (success is the common case, and the two verbs earn
/// different sentences). `error` is the transport/handler error rendered by the
/// caller's `Display`, when the dispatch failed.
pub fn takedown_verdict(restore: bool, error: Option<String>) -> LocalizedText {
    match error {
        Some(e) => LocalizedText::key_arg("admin.nest_page.takedown_failed", "error", e),
        None if restore => LocalizedText::key("admin.nest_page.takedown_restored"),
        None => LocalizedText::key("admin.nest_page.takedown_done"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(content_id: &str, reference: &str, restore: bool) -> TakedownForm {
        TakedownForm {
            content_id: content_id.into(),
            content_type: TakedownContentType::Post,
            legal_reference: reference.into(),
            restore,
        }
    }

    /// Decision 1: a citation-less takedown is never armable, and the reason
    /// names the missing citation (not the missing id).
    #[test]
    fn a_citationless_takedown_is_blocked_with_the_reference_reason() {
        let view = takedown_form_view(&form(&"ab".repeat(32), "", false));
        assert!(!view.can_submit);
        assert_eq!(
            view.blocked_reason.expect("blocked").key,
            "admin.nest_page.takedown_blocked_no_reference"
        );
    }

    /// Decision 2 — the guard's asymmetry: a restore with no note is armable.
    #[test]
    fn a_noteless_restore_is_armable() {
        let view = takedown_form_view(&form(&"ab".repeat(32), "", true));
        assert!(view.can_submit);
        assert!(view.blocked_reason.is_none());
        assert_eq!(view.arm_label.key, "admin.nest_page.takedown_arm_restore");
    }

    /// An empty content id blocks either verb — there is nothing to act on.
    #[test]
    fn an_empty_content_id_blocks_both_verbs() {
        for restore in [false, true] {
            let view = takedown_form_view(&form("   ", "Court order 42/2026", restore));
            assert!(!view.can_submit, "restore={restore}");
            assert_eq!(
                view.blocked_reason.expect("blocked").key,
                "admin.nest_page.takedown_blocked_no_content"
            );
        }
    }

    /// Decision 3: the takedown confirm names the content and the citation
    /// (trimmed as they will be sent), and the restore summary names the
    /// content without inventing a citation arg.
    #[test]
    fn the_confirm_summary_names_content_and_citation() {
        let id = "cd".repeat(32);
        let view = takedown_form_view(&form(&format!("  {id} "), " Court order 42/2026 ", false));
        assert!(view.can_submit);
        assert_eq!(
            view.confirm_summary.key,
            "admin.nest_page.takedown_confirm_takedown"
        );
        assert_eq!(view.confirm_summary.args["content_id"], id);
        assert_eq!(view.confirm_summary.args["content_type"], "post");
        assert_eq!(
            view.confirm_summary.args["reference"],
            "Court order 42/2026"
        );

        let restore = takedown_form_view(&form(&id, "", true));
        assert_eq!(
            restore.confirm_summary.key,
            "admin.nest_page.takedown_confirm_restore"
        );
        assert!(!restore.confirm_summary.args.contains_key("reference"));
    }

    /// Decision 4: the verdict wording — per verb on success, the error carried
    /// as a named arg on failure.
    #[test]
    fn the_verdict_words_each_outcome() {
        assert_eq!(
            takedown_verdict(false, None).key,
            "admin.nest_page.takedown_done"
        );
        assert_eq!(
            takedown_verdict(true, None).key,
            "admin.nest_page.takedown_restored"
        );
        let failed = takedown_verdict(false, Some("permission_denied".into()));
        assert_eq!(failed.key, "admin.nest_page.takedown_failed");
        assert_eq!(failed.args["error"], "permission_denied");
    }
}
