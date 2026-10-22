//! Whether a muted word collapses a notification row — the one decision every
//! app's list and every app-raised toast shares (`behavior/notifications.md`
//! § Muted words).
//!
//! The list is the user's sealed muted-keywords list, which the nest never
//! holds, so the decision runs client-side over what the row and the push
//! already carry. The matcher and its levels are the frame's
//! (`fauna_core::scoring::muted_keywords_collapse`); this module decides only
//! which text of a notification is matched and which rows are exempt.

use fauna_core::data::MutedKeyword;
use fauna_protocol::LocalizedText;
use fauna_protocol::notifications::{NotifItem, NotifType};
use fauna_protocol::push_events::{KnockPayload, NotificationPayload};

/// Does a muted word collapse this row behind its reveal?
pub fn notification_muted(muted: &[MutedKeyword], item: &NotifItem) -> bool {
    muted_over(muted, &item.notif_type, item.body.as_ref(), &item.summary)
}

/// The same decision for a `fauna.notification` push: an app withholds the
/// toast it would raise for a row that would collapse (rule 6) — a toast cannot
/// be revealed.
pub fn notification_push_muted(muted: &[MutedKeyword], push: &NotificationPayload) -> bool {
    muted_over(muted, &push.notif_type, push.body.as_ref(), &push.summary)
}

/// The same decision for a `fauna.knock` push: the knock toast paints the
/// knocker's message, so it is withheld for a knock whose row would collapse
/// (rule 6). A knock's `summary` is the knocker's raw message — the text a
/// bodyless knock is matched on, exactly as its row's would be.
pub fn knock_push_muted(muted: &[MutedKeyword], push: &KnockPayload) -> bool {
    muted_over(muted, &NotifType::Knock, push.body.as_ref(), &push.summary)
}

/// Rules 1, 2 and 5 over the fields the row and its push share.
///
/// A `security.notice` is exempt before any text is looked at: the list is
/// written by a User-class session, the very party a notice exposes. The text
/// matched is the relayed text the row carries — every `body.args` value, never
/// the catalog sentence around them — or, for a row with no `body`, the
/// `summary`, its only text. The args are joined by a newline, which no muted
/// term carries, so a match never straddles two args.
fn muted_over(
    muted: &[MutedKeyword],
    notif_type: &NotifType,
    body: Option<&LocalizedText>,
    summary: &str,
) -> bool {
    if muted.is_empty() || *notif_type == NotifType::SecurityNotice {
        return false;
    }
    let text = match body {
        Some(body) => body
            .args
            .values()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n"),
        None => summary.to_string(),
    };
    fauna_core::scoring::muted_keywords_collapse(muted, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::scoring::MutedKeywordLevel;

    fn hide(word: &str) -> MutedKeyword {
        MutedKeyword::new(word)
    }

    fn show_less(word: &str) -> MutedKeyword {
        MutedKeyword {
            keyword: word.into(),
            weight: MutedKeywordLevel::ShowLess.weight(),
        }
    }

    fn row(notif_type: &str, summary: &str, body: Option<LocalizedText>) -> NotifItem {
        NotifItem {
            notif_type: notif_type.into(),
            source: "fauna".into(),
            summary: summary.into(),
            body,
            ..Default::default()
        }
    }

    fn knock(message: &str) -> NotifItem {
        row(
            "knock",
            &format!("abcd1234 wants to connect: {message}"),
            Some(
                LocalizedText::new("notifications.row_knock")
                    .with_arg("sender", "abcd1234")
                    .with_arg("message", message),
            ),
        )
    }

    #[test]
    fn a_hide_word_in_an_arg_collapses_the_row() {
        assert!(notification_muted(
            &[hide("spoiler")],
            &knock("big SPOILER inside")
        ));
    }

    #[test]
    fn a_word_only_in_the_catalog_sentence_does_not_collapse() {
        // "connect" is in the English sentence (the summary), never in an arg.
        assert!(!notification_muted(&[hide("connect")], &knock("hello")));
    }

    #[test]
    fn a_show_less_word_changes_nothing() {
        assert!(!notification_muted(
            &[show_less("spoiler")],
            &knock("spoiler")
        ));
    }

    #[test]
    fn a_bodyless_row_matches_on_its_summary() {
        let item = row("like", "spoilerfan liked your post", None);
        assert!(notification_muted(&[hide("spoiler")], &item));
    }

    #[test]
    fn a_match_never_straddles_two_args() {
        let item = row(
            "like",
            "",
            Some(
                LocalizedText::new("notifications.row_like")
                    .with_arg("a", "spo")
                    .with_arg("b", "iler"),
            ),
        );
        assert!(!notification_muted(&[hide("spoiler")], &item));
    }

    #[test]
    fn a_security_notice_never_collapses() {
        let item = row(
            "security.notice",
            "New sign-in from 203.0.113.7",
            Some(
                LocalizedText::new("notifications.security_new_sign_in")
                    .with_arg("ip", "203.0.113.7"),
            ),
        );
        assert_eq!(item.notif_type, NotifType::SecurityNotice);
        assert!(!notification_muted(&[hide("203.0.113")], &item));
    }

    #[test]
    fn an_empty_list_collapses_nothing() {
        assert!(!notification_muted(&[], &knock("spoiler")));
    }

    #[test]
    fn a_knock_push_agrees_with_its_row() {
        for muted in [vec![hide("spoiler")], vec![hide("connect")], vec![]] {
            for message in ["a SPOILER here", "hello"] {
                let item = knock(message);
                let push = KnockPayload {
                    sender_id: "abcd1234".into(),
                    summary: message.into(),
                    body: item.body.clone(),
                    ..Default::default()
                };
                assert_eq!(
                    knock_push_muted(&muted, &push),
                    notification_muted(&muted, &item),
                    "knock push and row disagree for {muted:?} over {message:?}"
                );
            }
        }
        // A bodyless knock is matched on its summary — the knocker's message.
        let bare = KnockPayload {
            sender_id: "abcd1234".into(),
            summary: "a spoiler".into(),
            ..Default::default()
        };
        assert!(knock_push_muted(&[hide("spoiler")], &bare));
    }

    #[test]
    fn a_push_agrees_with_its_row() {
        let lists = [
            vec![hide("spoiler")],
            vec![show_less("spoiler")],
            vec![hide("connect")],
            vec![],
        ];
        let items = [
            knock("spoiler"),
            knock("hello"),
            row("like", "spoilerfan liked your post", None),
            row(
                "security.notice",
                "spoiler",
                Some(
                    LocalizedText::new("notifications.security_new_sign_in")
                        .with_arg("ip", "spoiler"),
                ),
            ),
        ];
        for muted in &lists {
            for item in &items {
                let push = NotificationPayload {
                    notification_id: item.id,
                    notif_type: item.notif_type.clone(),
                    source: item.source.clone(),
                    sender_id: None,
                    content_id: None,
                    summary: item.summary.clone(),
                    body: item.body.clone(),
                    timestamp: 1,
                    extra: Default::default(),
                };
                assert_eq!(
                    notification_push_muted(muted, &push),
                    notification_muted(muted, item),
                    "push and row disagree for {muted:?} over {item:?}"
                );
            }
        }
    }
}
