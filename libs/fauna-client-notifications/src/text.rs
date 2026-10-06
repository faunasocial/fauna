//! What a notification row says — the one decision every app shares
//! (`behavior/notifications.md` § Localized body).
//!
//! The nest sends the sentence twice: `NotifItem.body`, a catalog key plus
//! data args the app says in the reader's language, and `NotifItem.summary`,
//! the same sentence in English. [`notification_text`] picks between them and
//! **never resolves**: it hands back key + args, and each app puts them through
//! its own pipeline (`fauna_i18n` lookup on linux/tui, `L()` on web,
//! `getString`, `Bundle.localizedString`, RESW). Resolving here would strand
//! the five apps whose catalog is not the Rust one.

use fauna_core::localized::LocalizedText;
use fauna_protocol::notifications::NotifItem;
use fauna_protocol::push_events::{KnockPayload, NotificationPayload};

/// The catalog key painted when a row carries neither a usable body nor a
/// summary.
const DEFAULT_BODY_KEY: &str = "notifications.default_body";

/// The knock toast's own sentence (`{name} wants to connect`), painted when a
/// knock push carries no usable body (none sent, or a key this build
/// lacks). It is the toast's own sentence.
const KNOCK_FALLBACK_KEY: &str = "notifications.knock_body";

/// How much of the knocker's hex actor id a knock sentence names — the same
/// 8-hex prefix the nest puts in `notifications.row_knock`'s `sender` arg.
const KNOCK_SENDER_PREFIX: usize = 8;

/// The most characters of the knocker's message a knock toast carries. The nest
/// already refuses a summary over `MAX_KNOCK_SUMMARY_BYTES`; this cap is the
/// client's own defense in depth (a non-conforming or hostile peer nest).
const KNOCK_ARG_MAX_CHARS: usize = 200;

/// What an app renders for one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationText {
    /// Resolve through the app's native i18n pipeline.
    Localized(LocalizedText),
    /// Paint as-is: the nest's English `summary`.
    Verbatim(String),
}

/// Pick the row's text.
///
/// `body` wins only when its key is **in this build's catalog**. A key minted
/// by a newer nest is not, and falls back to `summary` — `LocalizedText`'s own
/// key-as-template fallback is a diagnostic for our machines' keys and would
/// paint `notifications.some_future_key` at the user. The catalog is the
/// generated one every app is built from, so "known to `fauna_i18n`" is "known
/// to this app".
pub fn notification_text(item: &NotifItem) -> NotificationText {
    decide(item.body.as_ref(), &item.summary)
}

/// The same decision for a `fauna.notification` push, whose `body` and
/// `summary` are the row's own: an OS-level notification an app raises off the
/// push says exactly what the row it announces says.
pub fn notification_push_text(push: &NotificationPayload) -> NotificationText {
    decide(push.body.as_ref(), &push.summary)
}

/// The same decision for a `fauna.knock` push — what an OS-level knock toast
/// says.
///
/// A catalog-known `body` localizes: it is the knock row's own
/// `notifications.row_knock` sentence, so the toast says what the row says.
/// Otherwise the toast's own sentence naming the sender. Unlike
/// `NotificationPayload.summary`, a knock's `summary` is the knocker's raw
/// message, not an English rendering of the row — it is never painted alone.
pub fn knock_push_text(push: &KnockPayload) -> NotificationText {
    if let Some(body) = push.body.as_ref()
        && fauna_i18n::strings::lookup(&body.key).is_some()
    {
        return sanitized_localized(body);
    }
    let sender: String = push.sender_id.chars().take(KNOCK_SENDER_PREFIX).collect();
    NotificationText::Localized(LocalizedText::key_args(
        KNOCK_FALLBACK_KEY,
        [("name".to_string(), sender)],
    ))
}

fn localized(body: &fauna_protocol::LocalizedText) -> NotificationText {
    NotificationText::Localized(LocalizedText::key_args(
        body.key.clone(),
        body.args.iter().map(|(k, v)| (k.clone(), v.clone())),
    ))
}

/// [`localized`] for a body whose args a stranger chose: every arg is reduced to
/// plain single-line text and capped
/// (`fauna_core::control_chars::sanitize_plain_line`), so no app's OS toast
/// paints control, bidi or unbounded text.
fn sanitized_localized(body: &fauna_protocol::LocalizedText) -> NotificationText {
    NotificationText::Localized(LocalizedText::key_args(
        body.key.clone(),
        body.args.iter().map(|(k, v)| {
            (
                k.clone(),
                fauna_core::control_chars::sanitize_plain_line(v, KNOCK_ARG_MAX_CHARS),
            )
        }),
    ))
}

fn decide(body: Option<&fauna_protocol::LocalizedText>, summary: &str) -> NotificationText {
    if let Some(body) = body
        && fauna_i18n::strings::lookup(&body.key).is_some()
    {
        return localized(body);
    }
    if summary.is_empty() {
        return NotificationText::Localized(LocalizedText::key(DEFAULT_BODY_KEY));
    }
    NotificationText::Verbatim(summary.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::LocalizedText as WireText;

    fn row(summary: &str, body: Option<WireText>) -> NotifItem {
        NotifItem {
            notif_type: "like".into(),
            source: "bluesky".into(),
            summary: summary.into(),
            body,
            ..Default::default()
        }
    }

    #[test]
    fn a_known_key_is_localized_with_its_args() {
        let item = row(
            "alice liked your post",
            Some(WireText::new("notifications.row_like").with_arg("sender", "alice")),
        );
        let NotificationText::Localized(text) = notification_text(&item) else {
            panic!("a catalog key must localize, not fall back to the English summary");
        };
        assert_eq!(text.key, "notifications.row_like");
        assert_eq!(
            text.resolve(fauna_i18n::strings::lookup),
            "alice liked your post"
        );
    }

    /// The still-newer-nest row of the compat table: never the raw key.
    #[test]
    fn a_key_this_build_does_not_have_falls_back_to_the_summary() {
        let item = row(
            "alice did a future thing",
            Some(WireText::new("notifications.row_not_minted_yet").with_arg("sender", "alice")),
        );
        assert_eq!(
            notification_text(&item),
            NotificationText::Verbatim("alice did a future thing".into())
        );
    }

    /// A row with no body (or a key unknown to this build).
    #[test]
    fn a_row_without_a_body_renders_its_summary() {
        assert_eq!(
            notification_text(&row("alice liked your post", None)),
            NotificationText::Verbatim("alice liked your post".into())
        );
    }

    #[test]
    fn neither_body_nor_summary_is_the_default_body_key() {
        assert_eq!(
            notification_text(&row("", None)),
            NotificationText::Localized(LocalizedText::key(DEFAULT_BODY_KEY))
        );
        assert!(fauna_i18n::strings::lookup(DEFAULT_BODY_KEY).is_some());
    }

    /// The push and the row it announces decide identically, arm for arm.
    #[test]
    fn a_push_says_what_its_row_says() {
        let bodies = [
            Some(WireText::new("notifications.row_like").with_arg("sender", "alice")),
            Some(WireText::new("notifications.row_not_minted_yet")),
            None,
        ];
        for body in bodies {
            for summary in ["alice liked your post", ""] {
                let push = NotificationPayload {
                    notification_id: 1,
                    notif_type: "like".into(),
                    source: "fauna".into(),
                    sender_id: None,
                    content_id: None,
                    summary: summary.into(),
                    body: body.clone(),
                    timestamp: 1,
                    extra: Default::default(),
                };
                assert_eq!(
                    notification_push_text(&push),
                    notification_text(&row(summary, body.clone())),
                    "push and row disagree for body {body:?}, summary {summary:?}"
                );
            }
        }
    }

    /// An unknown key with an empty summary still must not paint the key.
    #[test]
    fn an_unknown_key_over_an_empty_summary_is_the_default_body() {
        let item = row("", Some(WireText::new("notifications.row_not_minted_yet")));
        assert_eq!(
            notification_text(&item),
            NotificationText::Localized(LocalizedText::key(DEFAULT_BODY_KEY))
        );
    }

    const KNOCKER: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    fn knock(body: Option<WireText>) -> KnockPayload {
        KnockPayload {
            sender_id: KNOCKER.into(),
            summary: "hi, it's me".into(),
            body,
            ..Default::default()
        }
    }

    /// The body the nest mints for a knock (`routes.rs` `knock_body`: the
    /// sender arg is the 8-hex prefix).
    fn row_knock_body() -> WireText {
        WireText::new("notifications.row_knock")
            .with_arg("sender", &KNOCKER[..8])
            .with_arg("message", "hi, it's me")
    }

    #[test]
    fn a_knock_with_a_known_key_says_the_rows_sentence() {
        let NotificationText::Localized(text) = knock_push_text(&knock(Some(row_knock_body())))
        else {
            panic!("a catalog knock body must localize");
        };
        assert_eq!(
            text.resolve(fauna_i18n::strings::lookup),
            "a1b2c3d4 wants to connect: hi, it's me"
        );
    }

    /// The knock toast and the knock row it announces decide identically for
    /// a known key: same key, same args.
    #[test]
    fn a_knock_toast_agrees_with_its_row() {
        let row = NotifItem {
            notif_type: "knock".into(),
            source: "fauna".into(),
            summary: "a1b2c3d4 wants to connect: hi, it's me".into(),
            body: Some(row_knock_body()),
            ..Default::default()
        };
        assert_eq!(
            knock_push_text(&knock(Some(row_knock_body()))),
            notification_text(&row)
        );
    }

    /// The witness for the stranger's message: markup text, a newline, a bidi
    /// override and 10 KB all reach the toast as plain, single-line, capped text.
    #[test]
    fn a_knock_message_reaches_the_toast_as_plain_capped_text() {
        let hostile = format!(
            "\u{202E}\nAlice (your contact): <a href=x>go</a>\u{2028}{}",
            "x".repeat(10_240)
        );
        let body = WireText::new("notifications.row_knock")
            .with_arg("sender", &KNOCKER[..8])
            .with_arg("message", &hostile);
        let NotificationText::Localized(text) = knock_push_text(&knock(Some(body))) else {
            panic!("a catalog knock body must localize");
        };
        let message = &text.args["message"];
        assert!(!message.chars().any(|c| c.is_control()), "{message:?}");
        assert!(!message.contains(['\u{202E}', '\u{2028}']));
        assert!(message.starts_with("Alice (your contact): <a href=x>go</a> x"));
        assert_eq!(message.chars().count(), KNOCK_ARG_MAX_CHARS);
        assert_eq!(text.args["sender"], KNOCKER[..8]);
    }

    /// A key this build lacks never reaches the user as a raw key, and the
    /// knocker's raw message is never painted in its place.
    #[test]
    fn a_knock_with_an_unknown_key_is_the_toast_sentence() {
        let text = knock_push_text(&knock(Some(WireText::new(
            "notifications.row_not_minted_yet",
        ))));
        assert_eq!(
            text,
            NotificationText::Localized(LocalizedText::key_args(
                KNOCK_FALLBACK_KEY,
                [("name".to_string(), "a1b2c3d4".to_string())],
            ))
        );
    }

    /// A knock with no body: the toast's own sentence.
    #[test]
    fn a_knock_without_a_body_is_the_toast_sentence() {
        let NotificationText::Localized(text) = knock_push_text(&knock(None)) else {
            panic!("a bodyless knock paints the knock toast's catalog sentence");
        };
        assert_eq!(text.key, KNOCK_FALLBACK_KEY);
        assert_eq!(
            text.resolve(fauna_i18n::strings::lookup),
            "a1b2c3d4 wants to connect"
        );
    }
}
