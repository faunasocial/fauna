//! The `mail-settings` page's automatic-reply draft check
//! (`docs/goal/behavior/mail-policy-config.md` § Tier 3 — per-account;
//! `docs/goal/behavior/email-filters.md` § The auto-respond setting).
//!
//! The setting itself is `fauna-client-email`'s facade over one match-all
//! `AutoReply` filter rule; this is the one check every app runs on the message
//! draft before turning the reply on, the [`crate::forwarding`] shape.

use fauna_core::localized::LocalizedText;
use serde::Serialize;

/// Parse the automatic-reply message draft: returned trimmed, or refused when
/// blank — an automatic reply with no words in it is never what the user meant.
pub fn parse_autorespond_message_draft(draft: &str) -> Result<String, LocalizedText> {
    let message = draft.trim();
    if message.is_empty() {
        Err(LocalizedText::key(
            "mail_settings.autorespond_message_empty",
        ))
    } else {
        Ok(message.to_string())
    }
}

/// The outcome of checking the automatic-reply message draft, in a shape the
/// UniFFI and WASM boundaries can carry (neither can return a
/// `Result<_, LocalizedText>`): [`parse_autorespond_message_draft`] as data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AutorespondMessageDraftOutcome {
    /// A message (trimmed) — commit it with `autorespond_set`.
    Set { text: String },
    /// Refused; show `message` on the page and send nothing.
    Refused { message: LocalizedText },
}

/// [`parse_autorespond_message_draft`] as an exported outcome record.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn autorespond_message_draft_outcome(draft: &str) -> AutorespondMessageDraftOutcome {
    match parse_autorespond_message_draft(draft) {
        Ok(text) => AutorespondMessageDraftOutcome::Set { text },
        Err(message) => AutorespondMessageDraftOutcome::Refused { message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_draft_is_refused() {
        assert_eq!(
            parse_autorespond_message_draft("  \n "),
            Err(LocalizedText::key(
                "mail_settings.autorespond_message_empty"
            ))
        );
    }

    #[test]
    fn the_outcome_carries_the_trimmed_text_or_the_refusal() {
        assert_eq!(
            autorespond_message_draft_outcome(" Away. "),
            AutorespondMessageDraftOutcome::Set {
                text: "Away.".to_string()
            }
        );
        assert_eq!(
            autorespond_message_draft_outcome(""),
            AutorespondMessageDraftOutcome::Refused {
                message: LocalizedText::key("mail_settings.autorespond_message_empty")
            }
        );
    }

    #[test]
    fn a_message_is_trimmed_and_kept() {
        assert_eq!(
            parse_autorespond_message_draft("  Away until Monday.\n"),
            Ok("Away until Monday.".to_string())
        );
    }
}
