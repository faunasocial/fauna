//! The unsent-changes describer — an outbox intent in user words.
//!
//! Charter: `account-offline-mutation.md` § The offline-mutation contract →
//! *The unsent-changes surface*, point 4: the kind-in-user-words comes from a
//! per-kind describer registered beside the offline class
//! ([`crate::offline_class`]), and an unregistered kind describes as its kind
//! string — a dull row, never a missing one. Every app renders the list from
//! the same [`IntentDescription`], so none of them re-derives what an intent
//! was (priority #2).
//!
//! [`DESCRIBERS`] is the registry: one entry per kind that has a composer
//! today (an `*_or_queue` door that can put it in the outbox). The payload is
//! read leniently — only the fields the label needs, through serde's default
//! of ignoring the rest — and a payload that does not decode still describes,
//! by its kind's label with no target.
//!
//! The state and failure-reason texts live here too ([`state_text`],
//! [`reason_text`]): the stored `failure_reason` is a stable code, rendered
//! through a [`LocalizedText`] keyed by it, falling back to *"Your nest
//! refused this change (<code>)"*.

use fauna_core::localized::LocalizedText;
use fauna_core::offline_intent::{
    IntentDescription, IntentTarget, MLS_INTENT_KIND, UnsentChange, UnsentState, hold_code,
    park_code,
};
use serde::Deserialize;

/// How a describer reads one kind's payload.
pub type Describer = fn(&[u8]) -> IntentDescription;

/// The registry: every kind that has a composer today, and its describer.
/// A kind with no entry describes as its kind string ([`describe_intent`]).
pub const DESCRIBERS: &[(&str, Describer)] = &[
    ("fauna.posts.interact", describe_post_interact),
    (MLS_INTENT_KIND, describe_mls_send),
    ("fauna.family.approvals.decide", describe_approval_decide),
    ("fauna.family.contact.add", describe_contact_add),
    ("fauna.email.filters.create", describe_email_filter_create),
    ("fauna.bridges.add_follow", describe_bridge_follow_add),
    ("fauna.bridges.remove_follow", describe_bridge_follow_remove),
    ("fauna.knocks.accept", describe_knock_accept),
    (crate::contacts::KNOCK_SEND_KIND, describe_knock_send),
    (
        "fauna.moderation.abuse_report.submit",
        describe_abuse_report,
    ),
];

/// The longest message excerpt a list row carries, in characters.
pub const EXCERPT_CHARS: usize = 60;

const KEY_PREFIX: &str = "settings.unsent_changes.";

fn key(name: &str) -> String {
    format!("{KEY_PREFIX}{name}")
}

/// `kind`'s intent, described in user words from its queued `payload`. An
/// unregistered kind describes as its kind string (`kind_other`), with no
/// target.
pub fn describe_intent(kind: &str, payload: &[u8]) -> IntentDescription {
    match DESCRIBERS.iter().find(|(k, _)| *k == kind) {
        Some((_, describe)) => describe(payload),
        None => IntentDescription {
            label: LocalizedText::key_arg(key("kind_other"), "kind", kind),
            target: None,
        },
    }
}

/// The row's state in words.
pub fn state_text(state: &UnsentState) -> LocalizedText {
    LocalizedText::key(key(match state {
        UnsentState::Waiting => "state_waiting",
        UnsentState::Held => "state_held",
        UnsentState::Unsent { .. } => "state_unsent",
    }))
}

/// A held row's state in words, when [`UnsentChange::hold`] names what holds
/// it rather than its scope's park — the row's state line in place of
/// [`state_text`]'s *held*. A hold code this build does not name reads as
/// the plain held line.
pub fn hold_text(code: &str) -> LocalizedText {
    LocalizedText::key(key(match code {
        hold_code::CARRY_AWAITING_GROUP => "state_held_carry",
        _ => "state_held",
    }))
}

/// The state line of one unsent change: [`hold_text`] for a row that names
/// its hold, else [`state_text`].
pub fn change_state_text(change: &UnsentChange) -> LocalizedText {
    match (&change.state, change.hold.as_deref()) {
        (UnsentState::Held, Some(code)) => hold_text(code),
        (state, _) => state_text(state),
    }
}

/// A stored failure code in words — the drain's own park codes by name, any
/// other code (the nest's error code) through the fallback, which names it.
pub fn reason_text(code: &str) -> LocalizedText {
    let named = match code {
        park_code::UNKNOWN_KIND => Some("reason_unknown_kind"),
        park_code::UNDECODABLE => Some("reason_undecodable"),
        park_code::MLS_NO_CHANNEL => Some("reason_mls_no_channel"),
        park_code::MLS_NOT_A_MEMBER => Some("reason_mls_not_a_member"),
        park_code::MLS_SEND_FAILED => Some("reason_mls_send_failed"),
        park_code::MLS_ATTACHMENT_MISSING => Some("reason_mls_attachment_missing"),
        _ => None,
    };
    match named {
        Some(name) => LocalizedText::key(key(name)),
        None => LocalizedText::key_arg(key("reason_fallback"), "code", code),
    }
}

/// The head of a message body for a list row: whitespace collapsed to single
/// spaces, at most [`EXCERPT_CHARS`] characters, an ellipsis when cut.
pub fn excerpt(body: &str) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= EXCERPT_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(EXCERPT_CHARS).collect();
    cut.push('…');
    cut
}

#[derive(Deserialize)]
struct PostInteract {
    post_id: String,
    action: String,
    /// Rule 4's catch-all (transport.md § Schema and forward-compat discipline): this reads
    /// a few fields of a payload whose full type lives with its sender, so the rest is
    /// absorbed and ignored.
    #[serde(flatten, default)]
    #[allow(dead_code)]
    extra: std::collections::BTreeMap<String, crate::Value>,
}

fn describe_post_interact(payload: &[u8]) -> IntentDescription {
    let Ok(p) = fauna_cbor::decode_strict::<PostInteract>(payload) else {
        return IntentDescription {
            label: LocalizedText::key(key("kind_post_interaction")),
            target: None,
        };
    };
    let label = match p.action.as_str() {
        "like" => "kind_like",
        "unlike" => "kind_unlike",
        _ => "kind_post_interaction",
    };
    IntentDescription {
        label: LocalizedText::key(key(label)),
        target: Some(IntentTarget::Post {
            post_id: p.post_id,
            action: p.action,
        }),
    }
}

#[derive(Deserialize)]
struct MlsSend {
    channel_id: String,
    body: String,
    /// Rule 4's catch-all (transport.md § Schema and forward-compat discipline): this reads
    /// a few fields of a payload whose full type lives with its sender, so the rest is
    /// absorbed and ignored.
    #[serde(flatten, default)]
    #[allow(dead_code)]
    extra: std::collections::BTreeMap<String, crate::Value>,
}

fn describe_mls_send(payload: &[u8]) -> IntentDescription {
    let Ok(m) = fauna_cbor::decode_strict::<MlsSend>(payload) else {
        return IntentDescription {
            label: LocalizedText::key_arg(key("kind_message"), "excerpt", ""),
            target: None,
        };
    };
    let excerpt = excerpt(&m.body);
    IntentDescription {
        label: LocalizedText::key_arg(key("kind_message"), "excerpt", excerpt.clone()),
        target: Some(IntentTarget::Conversation {
            channel_id: m.channel_id,
            excerpt,
        }),
    }
}

#[derive(Deserialize)]
struct Decide {
    #[serde(default)]
    approve: bool,
    /// Rule 4's catch-all (transport.md § Schema and forward-compat discipline): this reads
    /// a few fields of a payload whose full type lives with its sender, so the rest is
    /// absorbed and ignored.
    #[serde(flatten, default)]
    #[allow(dead_code)]
    extra: std::collections::BTreeMap<String, crate::Value>,
}

fn describe_approval_decide(payload: &[u8]) -> IntentDescription {
    let approve = fauna_cbor::decode_strict::<Decide>(payload)
        .map(|d| d.approve)
        .unwrap_or(true);
    IntentDescription {
        label: LocalizedText::key(key(if approve {
            "kind_approval_approve"
        } else {
            "kind_approval_deny"
        })),
        target: None,
    }
}

fn describe_contact_add(_payload: &[u8]) -> IntentDescription {
    IntentDescription {
        label: LocalizedText::key(key("kind_contact_pre_approve")),
        target: None,
    }
}

#[derive(Deserialize)]
struct FilterCreate {
    name: String,
    /// Rule 4's catch-all (transport.md § Schema and forward-compat discipline): this reads
    /// a few fields of a payload whose full type lives with its sender, so the rest is
    /// absorbed and ignored.
    #[serde(flatten, default)]
    #[allow(dead_code)]
    extra: std::collections::BTreeMap<String, crate::Value>,
}

/// A queued filter create names its rule; it has no target to open — the rule
/// has no server id until the drain.
fn describe_email_filter_create(payload: &[u8]) -> IntentDescription {
    let name = fauna_cbor::decode_strict::<FilterCreate>(payload)
        .map(|f| f.name)
        .unwrap_or_default();
    IntentDescription {
        label: LocalizedText::key_arg(key("kind_email_filter_create"), "name", name),
        target: None,
    }
}

#[derive(Deserialize)]
struct FollowAdd {
    id: String,
    #[serde(default)]
    petname: Option<String>,
    /// Rule 4's catch-all (transport.md § Schema and forward-compat discipline): this reads
    /// a few fields of a payload whose full type lives with its sender, so the rest is
    /// absorbed and ignored.
    #[serde(flatten, default)]
    #[allow(dead_code)]
    extra: std::collections::BTreeMap<String, crate::Value>,
}

/// A queued follow names the account (its petname when the user gave one);
/// it has no target to open — the follow is not on the nest's list until the
/// drain.
fn describe_bridge_follow_add(payload: &[u8]) -> IntentDescription {
    let account = fauna_cbor::decode_strict::<FollowAdd>(payload)
        .map(|f| f.petname.filter(|p| !p.is_empty()).unwrap_or(f.id))
        .unwrap_or_default();
    IntentDescription {
        label: LocalizedText::key_arg(key("kind_bridge_follow_add"), "account", account),
        target: None,
    }
}

#[derive(Deserialize)]
struct FollowRemove {
    follow_id: String,
    /// Rule 4's catch-all, as on [`FollowAdd`].
    #[serde(flatten, default)]
    #[allow(dead_code)]
    extra: std::collections::BTreeMap<String, crate::Value>,
}

/// A queued unfollow names the account by the id the follow list carries.
fn describe_bridge_follow_remove(payload: &[u8]) -> IntentDescription {
    let account = fauna_cbor::decode_strict::<FollowRemove>(payload)
        .map(|f| f.follow_id)
        .unwrap_or_default();
    IntentDescription {
        label: LocalizedText::key_arg(key("kind_bridge_follow_remove"), "account", account),
        target: None,
    }
}

/// A queued knock accept: the sender is a bare actor id, which says nothing to
/// a person, so the label stands alone, with no target.
fn describe_knock_accept(_payload: &[u8]) -> IntentDescription {
    IntentDescription {
        label: LocalizedText::key(key("kind_knock_accept")),
        target: None,
    }
}

/// A queued knock send (a contact request): the label names the recipient's
/// actor id, which is all the queued inputs carry — the lookup's handle is not
/// stored. No target: the contact is not on the nest's roster until the drain.
fn describe_knock_send(payload: &[u8]) -> IntentDescription {
    let recipient = fauna_cbor::decode_strict::<crate::contacts::KnockSendIntent>(payload)
        .map(|k| k.recipient_actor_id)
        .unwrap_or_default();
    IntentDescription {
        label: LocalizedText::key_arg(key("kind_knock_send"), "recipient", recipient),
        target: None,
    }
}

/// A queued abuse report: the label stands alone — the subject is a content
/// id, and naming the reported content again in the list would re-surface
/// what the reporter just hid.
fn describe_abuse_report(_payload: &[u8]) -> IntentDescription {
    IntentDescription {
        label: LocalizedText::key(key("kind_abuse_report")),
        target: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::offline_class::{OfflineClass, offline_class};

    /// The kinds that have a composer today — the `*_or_queue` doors
    /// (`fauna-client-posts`, `fauna-client-family`) and the conversations
    /// manager's MLS send. A new composer adds its kind here AND a describer.
    const COMPOSED_KINDS: &[&str] = &[
        "fauna.posts.interact",
        "fauna.conversations.channel.send",
        "fauna.family.approvals.decide",
        "fauna.family.contact.add",
        "fauna.email.filters.create",
        "fauna.bridges.add_follow",
        "fauna.bridges.remove_follow",
        "fauna.knocks.accept",
        "fauna.inbox.send",
        "fauna.moderation.abuse_report.submit",
    ];

    /// The registry and the composed kinds are one set: every kind a composer
    /// can queue has a describer, and the registry names no kind nothing
    /// queues. Each one is `OfflineQueued` — the only class that rests in the
    /// outbox.
    #[test]
    fn every_composed_kind_has_a_describer_and_no_other_kind_does() {
        let registered: BTreeSet<&str> = DESCRIBERS.iter().map(|(k, _)| *k).collect();
        let composed: BTreeSet<&str> = COMPOSED_KINDS.iter().copied().collect();
        assert_eq!(registered, composed);
        assert_eq!(registered.len(), DESCRIBERS.len(), "one entry per kind");
        for kind in registered {
            assert_eq!(
                offline_class(kind),
                Some(OfflineClass::OfflineQueued),
                "{kind} is described, so it must be queueable"
            );
        }
    }

    /// The i18n keys the describer names all exist in `en.yaml` — a renamed
    /// string fails here, not as a blank row on an app.
    #[test]
    fn every_key_the_describer_names_is_a_string() {
        let names = [
            "kind_like",
            "kind_unlike",
            "kind_post_interaction",
            "kind_message",
            "kind_approval_approve",
            "kind_approval_deny",
            "kind_contact_pre_approve",
            "kind_email_filter_create",
            "kind_bridge_follow_add",
            "kind_bridge_follow_remove",
            "kind_knock_accept",
            "kind_knock_send",
            "kind_abuse_report",
            "kind_other",
            "state_waiting",
            "state_held",
            "state_unsent",
            "reason_unknown_kind",
            "reason_undecodable",
            "reason_mls_no_channel",
            "reason_mls_not_a_member",
            "reason_mls_send_failed",
            "reason_mls_attachment_missing",
            "state_held_carry",
            "reason_fallback",
        ];
        let en = include_str!("../../../i18n/strings/en.yaml");
        let section = en
            .split_once("\n  unsent_changes:\n")
            .expect("settings.unsent_changes exists")
            .1;
        for name in names {
            assert!(
                section.contains(&format!("\n    {name}: "))
                    || section.starts_with(&format!("    {name}: ")),
                "settings.unsent_changes.{name} is missing from en.yaml"
            );
        }
    }

    #[test]
    fn a_filter_create_describes_with_its_rule_name() {
        let payload = crate::encode_canonical(&crate::email::CreateEmailFilterRequest {
            name: "Move newsletters".into(),
            rules: vec![],
            combination: "all".into(),
            action: crate::email::EmailFilterAction::Allow,
            priority: 0,
            continue_on_match: false,
            extra: Default::default(),
        })
        .unwrap();
        let d = describe_intent("fauna.email.filters.create", &payload);
        assert_eq!(
            d.label,
            LocalizedText::key_arg(key("kind_email_filter_create"), "name", "Move newsletters")
        );
        assert_eq!(d.target, None);
    }

    #[test]
    fn a_follow_describes_with_its_petname_or_account_and_an_unfollow_with_its_account() {
        let add = |petname: Option<&str>| {
            crate::encode_canonical(&crate::bridges_ui::AddFollowRequest {
                bridge_id: "nostr".into(),
                id: "npub1alice".into(),
                petname: petname.map(Into::into),
                extra: None,
                unknown_keys: Default::default(),
            })
            .unwrap()
        };
        let named = describe_intent("fauna.bridges.add_follow", &add(Some("Alice")));
        assert_eq!(
            named.label,
            LocalizedText::key_arg(key("kind_bridge_follow_add"), "account", "Alice")
        );
        assert_eq!(named.target, None);
        let bare = describe_intent("fauna.bridges.add_follow", &add(None));
        assert_eq!(
            bare.label,
            LocalizedText::key_arg(key("kind_bridge_follow_add"), "account", "npub1alice")
        );

        let remove = crate::encode_canonical(&crate::bridges_ui::RemoveFollowRequest {
            bridge_id: "nostr".into(),
            follow_id: "npub1alice".into(),
            extra: Default::default(),
        })
        .unwrap();
        let d = describe_intent("fauna.bridges.remove_follow", &remove);
        assert_eq!(
            d.label,
            LocalizedText::key_arg(key("kind_bridge_follow_remove"), "account", "npub1alice")
        );
        assert_eq!(d.target, None);
    }

    #[test]
    fn a_knock_accept_and_a_report_describe_by_their_labels() {
        let d = describe_intent("fauna.knocks.accept", b"\xa0");
        assert_eq!(d.label, LocalizedText::key(key("kind_knock_accept")));
        assert_eq!(d.target, None);
        let d = describe_intent("fauna.moderation.abuse_report.submit", b"\xa0");
        assert_eq!(d.label, LocalizedText::key(key("kind_abuse_report")));
        assert_eq!(d.target, None);
    }

    #[test]
    fn a_knock_send_describes_with_its_recipient() {
        let payload = crate::encode_canonical(&crate::contacts::KnockSendIntent {
            recipient_actor_id: "ab".repeat(32),
            recipient_nest_url: None,
            extra: Default::default(),
        })
        .unwrap();
        let d = describe_intent("fauna.inbox.send", &payload);
        assert_eq!(
            d.label,
            LocalizedText::key_arg(key("kind_knock_send"), "recipient", "ab".repeat(32))
        );
        assert_eq!(d.target, None);
    }

    #[test]
    fn an_unregistered_kind_describes_as_its_kind_string() {
        let d = describe_intent("fauna.some.future_kind", b"\xa0");
        assert_eq!(
            d.label,
            LocalizedText::key_arg(
                "settings.unsent_changes.kind_other",
                "kind",
                "fauna.some.future_kind"
            )
        );
        assert_eq!(d.target, None);
    }

    #[test]
    fn a_like_and_an_unlike_describe_with_their_post() {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            post_id: &'a str,
            action: &'a str,
            body: Option<String>,
        }
        for (action, label) in [
            ("like", "kind_like"),
            ("unlike", "kind_unlike"),
            ("repost", "kind_post_interaction"),
        ] {
            let payload = fauna_cbor::encode_canonical(&Req {
                post_id: "aa",
                action,
                body: None,
            })
            .unwrap();
            let d = describe_intent("fauna.posts.interact", &payload);
            assert_eq!(d.label, LocalizedText::key(key(label)));
            assert_eq!(
                d.target,
                Some(IntentTarget::Post {
                    post_id: "aa".into(),
                    action: action.into()
                })
            );
        }
    }

    #[test]
    fn a_message_describes_with_its_channel_and_excerpt() {
        #[derive(serde::Serialize)]
        struct Intent<'a> {
            channel_id: &'a str,
            body: &'a str,
            composed_at_ms: i64,
        }
        let payload = fauna_cbor::encode_canonical(&Intent {
            channel_id: "cc",
            body: "see you\n  at noon",
            composed_at_ms: 1,
        })
        .unwrap();
        let d = describe_intent(MLS_INTENT_KIND, &payload);
        assert_eq!(
            d.label,
            LocalizedText::key_arg(key("kind_message"), "excerpt", "see you at noon")
        );
        assert_eq!(
            d.target,
            Some(IntentTarget::Conversation {
                channel_id: "cc".into(),
                excerpt: "see you at noon".into()
            })
        );
    }

    #[test]
    fn an_undecodable_payload_still_describes_by_its_kind() {
        let d = describe_intent("fauna.posts.interact", b"not cbor");
        assert_eq!(d.label, LocalizedText::key(key("kind_post_interaction")));
        assert_eq!(d.target, None);
    }

    #[test]
    fn a_long_body_is_cut_with_an_ellipsis() {
        let long = "x".repeat(EXCERPT_CHARS + 5);
        let cut = excerpt(&long);
        assert_eq!(cut.chars().count(), EXCERPT_CHARS + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn a_park_code_has_its_words_and_any_other_code_the_fallback() {
        assert_eq!(
            reason_text(park_code::MLS_NOT_A_MEMBER),
            LocalizedText::key(key("reason_mls_not_a_member"))
        );
        assert_eq!(
            reason_text("fauna.posts.not_found"),
            LocalizedText::key_arg(key("reason_fallback"), "code", "fauna.posts.not_found")
        );
        assert_eq!(
            state_text(&UnsentState::Held),
            LocalizedText::key(key("state_held"))
        );
    }
}
