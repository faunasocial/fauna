//! Sending to one of the account's own mailing lists from the conversations
//! compose form (`docs/goal/behavior/mail-mass-mailing.md` § Composing a list
//! message, § The per-day per-account cap).
//!
//! The user addresses a list by typing its address into the ordinary recipient
//! picker; nothing else about the compose form changes. When the one mail
//! recipient is one of the account's own lists, the SMTP rail sends through
//! the nest's list fan-out (`fauna.bridges.send_list_message`, via
//! [`crate::backend::OutboundMailSink::submit_to_list`]) instead of
//! `fauna.email.send`, and the compose state carries a [`ListSendView`] — the
//! three compose-form elements every app renders from it:
//!
//! - `dm-compose-list-send-warning` ← [`ListSendView::send_warning`]: how many
//!   subscribed recipients the send reaches and today's quota;
//! - `dm-compose-list-quota-warning` ← [`ListSendView::quota_warning`]: the
//!   approaching-limit warning (within 10% of the cap) or the over-limit
//!   explanation;
//! - `dm-compose-list-send-progress` ← [`ListSendView::progress`]: the newest
//!   send to the list as one whole-send progress, never per recipient.
//!
//! The view is derived here, once, from the wire facts ([`OwnMailLists`] +
//! [`ListSendProgress`]) — the apps only paint its texts (priority #2).

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

use crate::address::TypedAddress;

/// One of the account's own mailing lists, as the compose form needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnMailList {
    /// Lowercase hex of the 16-byte list id (what the list send carries).
    pub list_id_hex: String,
    /// The list's posting address, `<local_part>@<local_domain>`.
    pub address: String,
    /// The list's friendly name; the address when it has none.
    pub name: String,
    /// Subscribed members — the recipients a send reaches.
    pub member_count: u64,
}

/// The account's lists plus today's per-account list-recipient meter
/// (`fauna.bridges.list_account_lists`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnMailLists {
    pub lists: Vec<OwnMailList>,
    /// Recipients the account's list sends reserved today (UTC).
    pub recipients_today: u64,
    /// The per-account daily cap; 0 when the nest does not say (an older nest).
    pub recipients_per_day: u64,
}

impl OwnMailLists {
    /// The list `recipients` address, when they are exactly one mail address
    /// and it is one of these lists (case-insensitive — a mail address's
    /// domain is, and a list's local part is stored lowercase).
    pub fn target(&self, recipients: &[String]) -> Option<&OwnMailList> {
        let [only] = recipients else { return None };
        self.lists
            .iter()
            .find(|l| l.address.eq_ignore_ascii_case(only.trim()))
    }
}

/// The mail addresses a compose sends to, self dropped — the input
/// [`OwnMailLists::target`] matches on. Shared by the send and the view so the
/// two cannot disagree on which compose is a list send.
pub fn mail_recipients(addresses: &[TypedAddress], self_address: &str) -> Vec<String> {
    addresses
        .iter()
        .filter_map(|a| match a {
            TypedAddress::Email { email_address } => Some(email_address.clone()),
            _ => None,
        })
        .filter(|e| !e.eq_ignore_ascii_case(self_address))
        .collect()
}

/// The newest send to a list, as a whole (`fauna.bridges.list_list_send_history`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListSendProgress {
    pub recipient_count: u64,
    pub delivered_count: u64,
}

/// What the compose form shows for a compose addressed to one of the
/// account's own lists. `None` on [`crate::compose::ComposeState::list_send`]
/// for every other compose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ListSendView {
    /// The list's name, as the texts below name it.
    pub list_name: String,
    /// Subscribed recipients the next send reaches.
    pub member_count: u64,
    /// `dm-compose-list-send-warning`.
    pub send_warning: LocalizedText,
    /// `dm-compose-list-quota-warning` — shown only when today's remaining
    /// allowance is within 10% of the cap, or too small for this list.
    pub quota_warning: Option<LocalizedText>,
    /// `dm-compose-list-send-progress` — shown once the list has a send.
    pub progress: Option<LocalizedText>,
}

pub const SEND_WARNING_KEY: &str = "conversations.unified.list_send_warning";
pub const SEND_WARNING_NO_LIMIT_KEY: &str = "conversations.unified.list_send_warning_no_limit";
pub const QUOTA_APPROACHING_KEY: &str = "conversations.unified.list_quota_approaching";
pub const QUOTA_OVER_KEY: &str = "conversations.unified.list_quota_over";
pub const SEND_PROGRESS_KEY: &str = "conversations.unified.list_send_progress";

/// Build the compose form's view for `list` (§ Composing a list message,
/// step 2; § The per-day per-account cap: "Approaching daily limit — N more
/// recipients today" within 10% of the cap; an over-limit send is explained
/// where it was composed).
pub fn list_send_view(
    list: &OwnMailList,
    lists: &OwnMailLists,
    progress: Option<&ListSendProgress>,
) -> ListSendView {
    let count = list.member_count.to_string();
    let per_day = lists.recipients_per_day;
    let used = lists.recipients_today;
    let send_warning = if per_day == 0 {
        LocalizedText::key_args(
            SEND_WARNING_NO_LIMIT_KEY,
            [("count", count.clone()), ("list", list.name.clone())],
        )
    } else {
        LocalizedText::key_args(
            SEND_WARNING_KEY,
            [
                ("count", count.clone()),
                ("list", list.name.clone()),
                ("used", used.to_string()),
                ("limit", per_day.to_string()),
            ],
        )
    };
    let quota_warning = (per_day > 0).then(|| {
        let remaining = per_day.saturating_sub(used);
        if list.member_count > remaining {
            Some(LocalizedText::key_args(
                QUOTA_OVER_KEY,
                [
                    ("count", count.clone()),
                    ("limit", per_day.to_string()),
                    ("remaining", remaining.to_string()),
                ],
            ))
        } else if remaining.saturating_mul(10) <= per_day {
            Some(LocalizedText::key_arg(
                QUOTA_APPROACHING_KEY,
                "remaining",
                remaining.to_string(),
            ))
        } else {
            None
        }
    });
    let progress = progress.map(|p| {
        LocalizedText::key_args(
            SEND_PROGRESS_KEY,
            [
                ("list", list.name.clone()),
                ("delivered", p.delivered_count.to_string()),
                ("count", p.recipient_count.to_string()),
            ],
        )
    });
    ListSendView {
        list_name: list.name.clone(),
        member_count: list.member_count,
        send_warning,
        quota_warning: quota_warning.flatten(),
        progress,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn news(member_count: u64) -> OwnMailList {
        OwnMailList {
            list_id_hex: "a1".repeat(16),
            address: "news@example.com".into(),
            name: "Weekly news".into(),
            member_count,
        }
    }

    fn lists(today: u64, per_day: u64, member_count: u64) -> OwnMailLists {
        OwnMailLists {
            lists: vec![news(member_count)],
            recipients_today: today,
            recipients_per_day: per_day,
        }
    }

    #[test]
    fn only_a_lone_recipient_that_is_an_own_list_is_a_list_send() {
        let l = lists(0, 100, 3);
        assert!(l.target(&["News@Example.com".into()]).is_some());
        assert!(l.target(&["someone@example.com".into()]).is_none());
        assert!(
            l.target(&["news@example.com".into(), "x@example.net".into()])
                .is_none(),
            "a list beside other recipients is not a list send"
        );
        assert!(l.target(&[]).is_none());
    }

    #[test]
    fn mail_recipients_drops_self_and_non_mail_addresses() {
        let addrs = vec![
            TypedAddress::Email {
                email_address: "me@example.com".into(),
            },
            TypedAddress::Email {
                email_address: "news@example.com".into(),
            },
        ];
        assert_eq!(
            mail_recipients(&addrs, "ME@example.com"),
            vec!["news@example.com".to_string()]
        );
    }

    #[test]
    fn the_send_warning_names_the_reach_and_todays_quota() {
        let l = lists(5, 100, 3);
        let v = list_send_view(&l.lists[0], &l, None);
        assert_eq!(v.send_warning.key, SEND_WARNING_KEY);
        assert_eq!(v.send_warning.args["count"], "3");
        assert_eq!(v.send_warning.args["list"], "Weekly news");
        assert_eq!(v.send_warning.args["used"], "5");
        assert_eq!(v.send_warning.args["limit"], "100");
        assert_eq!(v.quota_warning, None, "95 left of 100 is not close");
        assert_eq!(v.progress, None, "no send yet");
    }

    #[test]
    fn an_unknown_cap_warns_with_the_reach_only() {
        let l = lists(0, 0, 3);
        let v = list_send_view(&l.lists[0], &l, None);
        assert_eq!(v.send_warning.key, SEND_WARNING_NO_LIMIT_KEY);
        assert_eq!(v.quota_warning, None);
    }

    #[test]
    fn within_ten_percent_of_the_cap_warns_how_many_are_left() {
        let l = lists(91, 100, 3);
        let v = list_send_view(&l.lists[0], &l, None);
        let w = v.quota_warning.expect("9 left of 100 is within 10%");
        assert_eq!(w.key, QUOTA_APPROACHING_KEY);
        assert_eq!(w.args["remaining"], "9");
    }

    #[test]
    fn a_send_that_would_pass_the_cap_is_explained() {
        let l = lists(98, 100, 3);
        let v = list_send_view(&l.lists[0], &l, None);
        let w = v.quota_warning.expect("3 recipients do not fit in 2");
        assert_eq!(w.key, QUOTA_OVER_KEY);
        assert_eq!(w.args["remaining"], "2");
        assert_eq!(w.args["limit"], "100");
        assert_eq!(w.args["count"], "3");
    }

    #[test]
    fn the_newest_send_shows_as_one_progress() {
        let l = lists(3, 100, 3);
        let p = ListSendProgress {
            recipient_count: 3,
            delivered_count: 2,
        };
        let v = list_send_view(&l.lists[0], &l, Some(&p));
        let progress = v.progress.expect("a send happened");
        assert_eq!(progress.key, SEND_PROGRESS_KEY);
        assert_eq!(progress.args["delivered"], "2");
        assert_eq!(progress.args["count"], "3");
    }
}
