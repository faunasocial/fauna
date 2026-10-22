//! The `mail-settings` page's quota readout and over-quota notice
//! (`docs/goal/behavior/imap-server.md` § Composition with submission quota).
//!
//! Both come from one `fauna.bridges.get_mail_quotas` read. The readout states
//! the mailbox quota alone — bytes and messages, each used of its limit — and
//! never the daily sending allowance: the two are different limits with
//! different remedies and are never conflated in one surface (the same doc's
//! § Don't do these). The notice is present only when a limit has run out, and
//! names which one, each with its own sentence. Shared so the seven apps state
//! the rule once (priority #2); the sizes are [`byte_size`] texts, resolved in
//! the reader's language before substitution — the
//! [`fauna_core::format::orphaned_store_label`] two-level shape. The daily
//! sending allowance has its own readout beside it
//! ([`mail_submission_quota_display`]).

use fauna_core::format::byte_size;
use fauna_core::localized::LocalizedText;
use fauna_protocol::bridge_routing::GetMailQuotasReply;
use serde::{Deserialize, Serialize};

/// The six counts of a `fauna.bridges.get_mail_quotas` reply, without the
/// wire's forward-compat catch-all — the record the UniFFI and WASM
/// boundaries carry (the reply itself is not one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailQuotas {
    pub storage_bytes_used: u64,
    pub storage_bytes_limit: u64,
    pub message_count_used: u32,
    pub message_count_limit: u32,
    /// Recipient units spent today (UTC day bucket).
    pub submission_used_today: u32,
    /// The effective daily recipient allowance.
    pub submission_max_per_day: u32,
}

impl From<&GetMailQuotasReply> for MailQuotas {
    fn from(reply: &GetMailQuotasReply) -> Self {
        Self {
            storage_bytes_used: reply.storage_bytes_used,
            storage_bytes_limit: reply.storage_bytes_limit,
            message_count_used: reply.message_count_used,
            message_count_limit: reply.message_count_limit,
            submission_used_today: reply.submission_used_today,
            submission_max_per_day: reply.submission_max_per_day,
        }
    }
}

/// The `mail-settings-mailbox-quota` text, before resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailQuotaDisplay {
    /// The readout sentence, carrying `{used}` and `{limit}` (filled from the
    /// two sizes below) plus `{messages}` and `{message_limit}` as plain args.
    pub label: LocalizedText,
    /// Bytes held.
    pub used: LocalizedText,
    /// The mailbox's byte limit.
    pub limit: LocalizedText,
}

/// The mailbox readout for `reply`.
pub fn mail_quota_display(reply: &GetMailQuotasReply) -> MailQuotaDisplay {
    mail_quota_display_of(reply.into())
}

/// [`mail_quota_display`] over the boundary record.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn mail_quota_display_of(quotas: MailQuotas) -> MailQuotaDisplay {
    let mut label = LocalizedText::key_arg(
        "mail_settings.mailbox_quota",
        "messages",
        quotas.message_count_used.to_string(),
    );
    label.args.insert(
        "message_limit".to_string(),
        quotas.message_count_limit.to_string(),
    );
    MailQuotaDisplay {
        label,
        used: byte_size(quotas.storage_bytes_used),
        limit: byte_size(quotas.storage_bytes_limit),
    }
}

/// [`mail_quota_display`] resolved to the finished readout text.
pub fn mail_quota_text<F, S>(reply: &GetMailQuotasReply, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    let display = mail_quota_display(reply);
    display
        .label
        .resolve(&lookup)
        .replace("{used}", &display.used.resolve(&lookup))
        .replace("{limit}", &display.limit.resolve(&lookup))
}

/// The `mail-settings-submission-quota` text, before resolution: recipients
/// spent today of the daily allowance, a different limit from the mailbox's.
pub fn mail_submission_quota_display(reply: &GetMailQuotasReply) -> LocalizedText {
    mail_submission_quota_display_of(reply.into())
}

/// [`mail_submission_quota_display`] over the boundary record.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn mail_submission_quota_display_of(quotas: MailQuotas) -> LocalizedText {
    let mut label = LocalizedText::key_arg(
        "mail_settings.submission_quota",
        "used",
        quotas.submission_used_today.to_string(),
    );
    label.args.insert(
        "limit".to_string(),
        quotas.submission_max_per_day.to_string(),
    );
    label
}

/// [`mail_submission_quota_display`] resolved to the finished readout text.
pub fn mail_submission_quota_text<F, S>(reply: &GetMailQuotasReply, lookup: F) -> String
where
    F: Fn(&str) -> Option<S>,
    S: AsRef<str>,
{
    mail_submission_quota_display(reply).resolve(&lookup)
}

/// The `mail-settings-over-quota-notice` sentence, or `None` while no limit has
/// run out. A full mailbox (bytes or messages at or past the limit) is named
/// first: it is the one that loses incoming mail (the sender bounces). A limit
/// of 0 reads as "no limit" and never fires.
pub fn mail_over_quota_notice(reply: &GetMailQuotasReply) -> Option<LocalizedText> {
    mail_over_quota_notice_of(reply.into())
}

/// [`mail_over_quota_notice`] over the boundary record.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn mail_over_quota_notice_of(quotas: MailQuotas) -> Option<LocalizedText> {
    let spent = |used: u64, limit: u64| limit > 0 && used >= limit;
    if spent(quotas.storage_bytes_used, quotas.storage_bytes_limit)
        || spent(
            u64::from(quotas.message_count_used),
            u64::from(quotas.message_count_limit),
        )
    {
        Some(LocalizedText::key("mail_settings.over_quota_mailbox"))
    } else if spent(
        u64::from(quotas.submission_used_today),
        u64::from(quotas.submission_max_per_day),
    ) {
        Some(LocalizedText::key_arg(
            "mail_settings.over_quota_sending",
            "limit",
            quotas.submission_max_per_day.to_string(),
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(bytes: (u64, u64), messages: (u32, u32), sent: (u32, u32)) -> GetMailQuotasReply {
        GetMailQuotasReply {
            storage_bytes_used: bytes.0,
            storage_bytes_limit: bytes.1,
            message_count_used: messages.0,
            message_count_limit: messages.1,
            submission_used_today: sent.0,
            submission_max_per_day: sent.1,
            ..Default::default()
        }
    }

    fn lookup(key: &str) -> Option<&'static str> {
        match key {
            "mail_settings.mailbox_quota" => {
                Some("{used} of {limit} · {messages} of {message_limit} messages")
            }
            "size.bytes" => Some("{value} B"),
            "size.kb" => Some("{value} KB"),
            "size.mb" => Some("{value} MB"),
            _ => None,
        }
    }

    #[test]
    fn the_readout_states_the_mailbox_alone() {
        let text = mail_quota_text(
            &reply((2048, 5 * 1024 * 1024), (3, 1000), (7, 1000)),
            lookup,
        );
        assert_eq!(text, "2 KB of 5 MB · 3 of 1000 messages");
    }

    #[test]
    fn the_sending_readout_states_the_daily_allowance_alone() {
        let lookup = |key: &str| {
            (key == "mail_settings.submission_quota")
                .then_some("{used} of {limit} recipients today")
        };
        let text = mail_submission_quota_text(
            &reply((2048, 5 * 1024 * 1024), (3, 1000), (7, 500)),
            lookup,
        );
        assert_eq!(text, "7 of 500 recipients today");
    }

    #[test]
    fn no_notice_while_every_limit_has_room() {
        assert_eq!(
            mail_over_quota_notice(&reply((1, 10), (1, 10), (1, 10))),
            None
        );
    }

    #[test]
    fn a_full_mailbox_by_bytes_or_messages_is_named() {
        let full = Some(LocalizedText::key("mail_settings.over_quota_mailbox"));
        assert_eq!(
            mail_over_quota_notice(&reply((10, 10), (1, 10), (1, 10))),
            full
        );
        assert_eq!(
            mail_over_quota_notice(&reply((1, 10), (11, 10), (1, 10))),
            full
        );
        // The mailbox outranks a spent sending allowance.
        assert_eq!(
            mail_over_quota_notice(&reply((10, 10), (1, 10), (10, 10))),
            full
        );
    }

    #[test]
    fn a_spent_sending_allowance_is_named_with_its_limit() {
        assert_eq!(
            mail_over_quota_notice(&reply((1, 10), (1, 10), (1000, 1000))),
            Some(LocalizedText::key_arg(
                "mail_settings.over_quota_sending",
                "limit",
                "1000".to_string()
            ))
        );
    }

    #[test]
    fn a_zero_limit_never_fires() {
        assert_eq!(mail_over_quota_notice(&reply((5, 0), (5, 0), (5, 0))), None);
    }
}
