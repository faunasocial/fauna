//! The admin block's shared decision logic (`moderation.md` § Admin block of a
//! reported item) — which queue rows offer the verb, the armed confirm's
//! summary, the verdict wording, and the blocked list's rows, derived once here
//! rather than seven times (priority #2). The takedown console's
//! [`crate::takedown`] module is the shape this mirrors.
//!
//! Four decisions live here because each is one an app would get wrong alone:
//!
//! 1. **A block is of one item.** The verb arms only for a post or a message
//!    subject; an `actor` report has the account levers (`admin.md`), and a
//!    subject kind this build cannot read offers nothing. The disarmed verb
//!    states why instead of painting a dead control.
//! 2. **The confirm names what it does.** The armed summary carries the verb,
//!    the item and the report's reason — a lever on another user's content is
//!    never confirmed blind.
//! 3. **The verdict wording.** Block and unblock earn different sentences: an
//!    unblock re-serves the same bytes and keeps the block on record.
//! 4. **The blocked list's row.** Subject, reason and age, worded once, so the
//!    list every app paints below the queue reads the same.
//!
//! Two of the summary's arguments are themselves i18n keys (the item's kind
//! word and the reason's localized name): render the summary through
//! `LocalizedText::resolve_nested` or the platform's nested lookup.

use fauna_core::localized::LocalizedText;
use fauna_protocol::moderation::{AbuseReportReason, AbuseReportSubject, AdminBlockEntry};
use serde::{Deserialize, Serialize};

/// The `fauna.moderation.admin_block` reply's `content_type` vocabulary for a
/// subject — `"post"` / `"conversation"`, the takedown console's words —
/// or `None` for a subject a block cannot name (decision 1).
pub fn block_content_type(subject: &AbuseReportSubject) -> Option<&'static str> {
    match subject {
        AbuseReportSubject::Post { .. } => Some("post"),
        AbuseReportSubject::Message { .. } => Some("conversation"),
        AbuseReportSubject::Actor { .. } | AbuseReportSubject::Unknown(_) => None,
    }
}

/// What the Block / Unblock control renders for one subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockFormView {
    /// Whether the arm control may be offered (decision 1).
    pub can_submit: bool,
    /// Why not, when it may not.
    pub blocked_reason: Option<LocalizedText>,
    /// The arm control's own label — the verb flips with `restore`.
    pub arm_label: LocalizedText,
    /// The confirm control's label (verb-matched).
    pub confirm_label: LocalizedText,
}

/// Fold a report's subject into the verb's render decisions. `restore` is the
/// blocked list's Unblock; `false` is a queue row's Block.
pub fn block_form_view(subject: &AbuseReportSubject, restore: bool) -> BlockFormView {
    let blocked_reason = block_content_type(subject)
        .is_none()
        .then(|| LocalizedText::key("admin.nest_page.block_blocked_not_item"));
    let (arm_key, confirm_key) = if restore {
        (
            "admin.nest_page.block_arm_unblock",
            "admin.nest_page.block_confirm_button_unblock",
        )
    } else {
        (
            "admin.nest_page.block_arm_block",
            "admin.nest_page.block_confirm_button_block",
        )
    };
    BlockFormView {
        can_submit: blocked_reason.is_none(),
        blocked_reason,
        arm_label: LocalizedText::key(arm_key),
        confirm_label: LocalizedText::key(confirm_key),
    }
}

/// The i18n key of a subject's kind word (`a post` / `a message`).
fn item_key(subject: &AbuseReportSubject) -> &'static str {
    match subject {
        AbuseReportSubject::Message { .. } => "admin.nest_page.block_item_message",
        _ => "admin.nest_page.block_item_post",
    }
}

/// The armed confirm's decision surface (decision 2): the verb, the item (its
/// kind word and id) and the report's reason, before anything dispatches.
pub fn block_confirm_summary(
    subject: &AbuseReportSubject,
    reason: AbuseReportReason,
    restore: bool,
) -> LocalizedText {
    let key = if restore {
        "admin.nest_page.block_confirm_unblock"
    } else {
        "admin.nest_page.block_confirm_block"
    };
    LocalizedText::key_args(
        key,
        [
            ("item", item_key(subject).to_string()),
            ("content_id", subject.id().to_string()),
            ("reason", crate::report::reason_label(reason).key),
        ],
    )
}

/// The outcome line after a dispatch (decision 3) — deliberately not
/// `error-message` material on success. `error` is the transport/handler error
/// rendered by the caller's `Display`, when the dispatch failed.
pub fn block_verdict(restore: bool, error: Option<String>) -> LocalizedText {
    match error {
        Some(e) => LocalizedText::key_arg("admin.nest_page.block_failed", "error", e),
        None if restore => LocalizedText::key("admin.nest_page.block_restored"),
        None => LocalizedText::key("admin.nest_page.block_done"),
    }
}

/// One `admin-nest-blocked-item` row, worded (decision 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedRowView {
    /// The report the Unblock dispatch names.
    pub report_id: String,
    /// The subject's stored kind token (`post` / `message`).
    pub subject_kind: String,
    pub subject_id: String,
    /// The report's reason, as the queue row words it.
    pub reason: LocalizedText,
    /// Microsecond timestamp of the block — the app renders the age with its
    /// own relative-time helper, as for a queue row's `created_at`.
    pub blocked_at: i64,
    /// Whether Unblock may be offered — every listed item's may.
    pub unblock: BlockFormView,
}

pub fn blocked_row_view(entry: &AdminBlockEntry) -> BlockedRowView {
    BlockedRowView {
        report_id: entry.report_id.clone(),
        subject_kind: entry.subject.kind().to_string(),
        subject_id: entry.subject.id().to_string(),
        reason: crate::report::reason_label(entry.reason),
        blocked_at: entry.blocked_at,
        unblock: block_form_view(&entry.subject, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn post() -> AbuseReportSubject {
        AbuseReportSubject::Post {
            cid: "ab".repeat(32),
        }
    }

    fn message() -> AbuseReportSubject {
        AbuseReportSubject::Message {
            channel: "cd".repeat(32),
            record_cid: "ef".repeat(32),
        }
    }

    fn actor() -> AbuseReportSubject {
        AbuseReportSubject::Actor {
            actor_id: "01".repeat(32),
        }
    }

    /// Decision 1: a post and a message arm; an account never does, and says
    /// why.
    #[test]
    fn only_an_item_subject_arms_the_block() {
        for subject in [post(), message()] {
            let view = block_form_view(&subject, false);
            assert!(view.can_submit, "{subject:?}");
            assert!(view.blocked_reason.is_none());
            assert_eq!(view.arm_label.key, "admin.nest_page.block_arm_block");
        }
        let view = block_form_view(&actor(), false);
        assert!(!view.can_submit);
        assert_eq!(
            view.blocked_reason.expect("blocked").key,
            "admin.nest_page.block_blocked_not_item"
        );
        assert_eq!(block_content_type(&post()), Some("post"));
        assert_eq!(block_content_type(&message()), Some("conversation"));
        assert_eq!(block_content_type(&actor()), None);
    }

    /// Decision 2: the confirm names the verb, the item and the reason, and the
    /// reason reads in the viewer's language through a nested resolve.
    #[test]
    fn the_confirm_names_verb_item_and_reason() {
        let s = block_confirm_summary(&message(), AbuseReportReason::Harassment, false);
        assert_eq!(s.key, "admin.nest_page.block_confirm_block");
        assert_eq!(
            s.args.get("item").map(String::as_str),
            Some("admin.nest_page.block_item_message")
        );
        assert_eq!(
            s.args.get("content_id").map(String::as_str),
            Some("ef".repeat(32).as_str())
        );
        assert_eq!(
            s.args.get("reason").map(String::as_str),
            Some("moderation.report.reason.harassment")
        );
        let lookup = |k: &str| match k {
            "admin.nest_page.block_confirm_block" => Some("Block {item} {content_id}: {reason}?"),
            "admin.nest_page.block_item_message" => Some("message"),
            "moderation.report.reason.harassment" => Some("Harassment"),
            _ => None,
        };
        assert_eq!(
            s.resolve_nested(lookup),
            format!("Block message {}: Harassment?", "ef".repeat(32))
        );
        let u = block_confirm_summary(&post(), AbuseReportReason::Spam, true);
        assert_eq!(u.key, "admin.nest_page.block_confirm_unblock");
        assert_eq!(
            u.args.get("item").map(String::as_str),
            Some("admin.nest_page.block_item_post")
        );
    }

    /// Decision 3: the two verbs and a failure each have their own sentence.
    #[test]
    fn the_verdict_wording_follows_the_verb() {
        assert_eq!(block_verdict(false, None).key, "admin.nest_page.block_done");
        assert_eq!(
            block_verdict(true, None).key,
            "admin.nest_page.block_restored"
        );
        let f = block_verdict(false, Some("not_found".into()));
        assert_eq!(f.key, "admin.nest_page.block_failed");
        assert_eq!(f.args.get("error").map(String::as_str), Some("not_found"));
    }

    /// Decision 4: a blocked row carries what Unblock needs and arms it.
    #[test]
    fn a_blocked_row_offers_unblock() {
        let row = blocked_row_view(&AdminBlockEntry {
            report_id: "r1".into(),
            subject: post(),
            reason: AbuseReportReason::Hate,
            blocked_at: 42,
            extra: BTreeMap::new(),
        });
        assert_eq!(row.report_id, "r1");
        assert_eq!(row.subject_kind, "post");
        assert_eq!(row.subject_id, "ab".repeat(32));
        assert_eq!(row.reason.key, "moderation.report.reason.hate");
        assert_eq!(row.blocked_at, 42);
        assert!(row.unblock.can_submit);
        assert_eq!(
            row.unblock.arm_label.key,
            "admin.nest_page.block_arm_unblock"
        );
    }
}
