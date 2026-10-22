//! The per-account auto-respond setting (`mail-policy-config.md` § Tier 3,
//! `mail.account.autorespond_enabled` / `autorespond_message`) as a facade over
//! ONE match-all `AutoReply` filter rule — there is no nest-side auto-respond
//! state and no second enforcement path; the MTA's existing filter evaluation
//! sends the reply. The rule's shape, how the facade recognises it among the
//! user's own rules, and the fixed values it writes are ruled in
//! `docs/goal/behavior/email-filters.md` § The auto-respond setting.

use fauna_protocol::RpcRequester;
use fauna_protocol::email::{
    CreateEmailFilterReply, CreateEmailFilterRequest, EmailFilter, EmailFilterAction,
    UpdateEmailFilterReply, UpdateEmailFilterRequest,
};

use crate::EmailClient;

/// The stored name of the facade's rule — the nest refuses an empty name, and
/// the filter list shows it. A constant, not a knob.
pub const AUTORESPOND_RULE_NAME: &str = "Auto-respond";

/// The `Subject:` every auto-reply the facade's rule sends carries. A constant,
/// not a knob: the setting is one toggle and one message.
pub const AUTORESPOND_SUBJECT: &str = "Automatic reply";

/// At most one reply per `(recipient, sender)` per this many hours — 7 days,
/// the Sieve `vacation` default (RFC 5230 § 4.1). A constant, not a knob.
pub const AUTORESPOND_INTERVAL_HOURS: u32 =
    fauna_protocol::email::DEFAULT_AUTO_REPLY_INTERVAL_HOURS;

/// The facade's rule sorts before every other rule (evaluation is
/// `priority ASC, id ASC`), so a user's terminal rule can never shadow it.
pub const AUTORESPOND_PRIORITY: i32 = i32::MIN;

/// What the auto-respond toggle and message show.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutoRespond {
    pub enabled: bool,
    /// The reply body; empty when `enabled` is false.
    pub message: String,
}

/// Whether `filter` is an auto-respond rule: no conditions under `all` (it
/// matches every message) with an `AutoReply` action. Recognised by shape, not
/// name, so a match-all auto-reply the user built by hand IS the setting — it
/// does exactly what the toggle means.
pub fn is_autorespond_rule(filter: &EmailFilter) -> bool {
    filter.rules.is_empty()
        && filter.combination == "all"
        && matches!(filter.action, EmailFilterAction::AutoReply { .. })
}

/// The auto-respond rules among `filters`, in evaluation order (`priority ASC,
/// id ASC`); the first is the one the setting reads and keeps.
fn autorespond_rules(filters: &[EmailFilter]) -> Vec<&EmailFilter> {
    let mut rules: Vec<&EmailFilter> = filters.iter().filter(|f| is_autorespond_rule(f)).collect();
    rules.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.id.cmp(&b.id)));
    rules
}

/// The setting as `filters` hold it: on, with the first auto-respond rule's
/// body, or off. A conditional `AutoReply` rule is the user's own and does not
/// count.
pub fn autorespond_from_filters(filters: &[EmailFilter]) -> AutoRespond {
    match autorespond_rules(filters).first().map(|f| &f.action) {
        Some(EmailFilterAction::AutoReply { body, .. }) => AutoRespond {
            enabled: true,
            message: body.clone(),
        },
        _ => AutoRespond::default(),
    }
}

/// The action the facade writes for `message`.
fn autorespond_action(message: &str) -> EmailFilterAction {
    EmailFilterAction::AutoReply {
        subject: AUTORESPOND_SUBJECT.to_string(),
        body: message.to_string(),
        interval_hours: AUTORESPOND_INTERVAL_HOURS,
    }
}

impl<R: RpcRequester> EmailClient<R> {
    /// Read the auto-respond setting from the caller's filter rules.
    pub async fn autorespond_get(&self) -> Result<AutoRespond, R::Error> {
        Ok(autorespond_from_filters(&self.filters_list().await?))
    }

    /// Turn auto-respond on with `message`, or change the message. Creates the
    /// rule when none exists; otherwise rewrites the first auto-respond rule to
    /// the canonical shape (keeping its id) and deletes any further ones, so the
    /// account ends with exactly one.
    pub async fn autorespond_set(&self, message: &str) -> Result<(), R::Error> {
        let filters = self.filters_list().await?;
        let rules = autorespond_rules(&filters);
        match rules.split_first() {
            None => {
                let _: CreateEmailFilterReply = self
                    .nest
                    .request(
                        "fauna.email.filters.create",
                        CreateEmailFilterRequest {
                            name: AUTORESPOND_RULE_NAME.to_string(),
                            rules: vec![],
                            combination: "all".to_string(),
                            action: autorespond_action(message),
                            priority: AUTORESPOND_PRIORITY,
                            // Non-terminal: the reply composes with whatever the
                            // user's own rules then decide.
                            continue_on_match: true,
                            extra: Default::default(),
                        },
                    )
                    .await?;
            }
            Some((kept, extras)) => {
                let _: UpdateEmailFilterReply = self
                    .nest
                    .request(
                        "fauna.email.filters.update",
                        UpdateEmailFilterRequest {
                            id: kept.id,
                            name: AUTORESPOND_RULE_NAME.to_string(),
                            rules: vec![],
                            combination: "all".to_string(),
                            action: autorespond_action(message),
                            priority: AUTORESPOND_PRIORITY,
                            continue_on_match: true,
                            extra: Default::default(),
                        },
                    )
                    .await?;
                for extra in extras {
                    self.filters_delete(extra.id).await?;
                }
            }
        }
        Ok(())
    }

    /// Turn auto-respond off: delete every auto-respond rule (a filter has no
    /// disabled state). The message goes with the rule.
    pub async fn autorespond_clear(&self) -> Result<(), R::Error> {
        let filters = self.filters_list().await?;
        for rule in autorespond_rules(&filters) {
            self.filters_delete(rule.id).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{ScriptedRequester, block_on};
    use fauna_protocol::email::{
        DeleteEmailFilterReply, DeleteEmailFilterRequest, EmailFilterRule, ListEmailFiltersReply,
    };
    use std::sync::Arc;

    fn filter(id: i64, rules: Vec<EmailFilterRule>, action: EmailFilterAction) -> EmailFilter {
        EmailFilter {
            id,
            name: format!("rule {id}"),
            rules,
            combination: "all".into(),
            action,
            priority: 0,
            continue_on_match: false,
            created_at: 0,
            extra: Default::default(),
        }
    }

    fn reply(body: &str) -> EmailFilterAction {
        EmailFilterAction::AutoReply {
            subject: "Away".into(),
            body: body.into(),
            interval_hours: 24,
        }
    }

    fn sender_is(address: &str) -> EmailFilterRule {
        EmailFilterRule::SenderIs {
            address: address.into(),
        }
    }

    fn list(filters: Vec<EmailFilter>) -> Vec<u8> {
        fauna_protocol::encode_canonical(&ListEmailFiltersReply {
            filters,
            extra: Default::default(),
        })
        .unwrap()
        .to_vec()
    }

    fn created() -> Vec<u8> {
        fauna_protocol::encode_canonical(&CreateEmailFilterReply {
            id: 99,
            extra: Default::default(),
        })
        .unwrap()
        .to_vec()
    }

    fn updated() -> Vec<u8> {
        fauna_protocol::encode_canonical(&UpdateEmailFilterReply {
            ok: true,
            extra: Default::default(),
        })
        .unwrap()
        .to_vec()
    }

    fn deleted() -> Vec<u8> {
        fauna_protocol::encode_canonical(&DeleteEmailFilterReply {
            ok: true,
            extra: Default::default(),
        })
        .unwrap()
        .to_vec()
    }

    fn client(
        replies: Vec<Vec<u8>>,
    ) -> (Arc<ScriptedRequester>, EmailClient<Arc<ScriptedRequester>>) {
        let rec = Arc::new(ScriptedRequester::new(replies));
        (rec.clone(), EmailClient::new(rec))
    }

    fn assert_canonical(
        name: &str,
        rules: &[EmailFilterRule],
        combination: &str,
        action: &EmailFilterAction,
        priority: i32,
        continue_on_match: bool,
        message: &str,
    ) {
        assert_eq!(name, AUTORESPOND_RULE_NAME);
        assert!(rules.is_empty(), "matches every message");
        assert_eq!(combination, "all");
        assert_eq!(action, &autorespond_action(message));
        assert_eq!(priority, AUTORESPOND_PRIORITY);
        assert!(
            continue_on_match,
            "never terminal, never shadows a user rule"
        );
    }

    #[test]
    fn toggle_on_creates_one_match_all_rule() {
        let user_rule = filter(
            1,
            vec![sender_is("a@example.com")],
            EmailFilterAction::Discard,
        );
        let (rec, c) = client(vec![list(vec![user_rule]), created()]);
        block_on(c.autorespond_set("I am away.")).unwrap();
        assert_eq!(
            rec.kinds(),
            vec!["fauna.email.filters.list", "fauna.email.filters.create"]
        );
        let req: CreateEmailFilterRequest =
            fauna_protocol::decode_strict(&rec.payloads()[1]).unwrap();
        assert_canonical(
            &req.name,
            &req.rules,
            &req.combination,
            &req.action,
            req.priority,
            req.continue_on_match,
            "I am away.",
        );
        assert_eq!(rec.remaining(), 0);
    }

    #[test]
    fn editing_the_message_rewrites_the_rule_in_place() {
        let mut ours = filter(5, vec![], reply("Old text"));
        ours.priority = AUTORESPOND_PRIORITY;
        ours.continue_on_match = true;
        let (rec, c) = client(vec![list(vec![ours]), updated()]);
        block_on(c.autorespond_set("New text")).unwrap();
        assert_eq!(
            rec.kinds(),
            vec!["fauna.email.filters.list", "fauna.email.filters.update"]
        );
        let req: UpdateEmailFilterRequest =
            fauna_protocol::decode_strict(&rec.payloads()[1]).unwrap();
        assert_eq!(req.id, 5);
        assert_canonical(
            &req.name,
            &req.rules,
            &req.combination,
            &req.action,
            req.priority,
            req.continue_on_match,
            "New text",
        );
    }

    #[test]
    fn toggle_off_deletes_the_rule_and_leaves_user_rules_alone() {
        let user_rule = filter(1, vec![sender_is("a@example.com")], reply("Hi Alice"));
        let ours = filter(5, vec![], reply("Away"));
        let (rec, c) = client(vec![list(vec![user_rule, ours]), deleted()]);
        block_on(c.autorespond_clear()).unwrap();
        assert_eq!(
            rec.kinds(),
            vec!["fauna.email.filters.list", "fauna.email.filters.delete"]
        );
        let req: DeleteEmailFilterRequest =
            fauna_protocol::decode_strict(&rec.payloads()[1]).unwrap();
        assert_eq!(req.id, 5);
    }

    #[test]
    fn toggle_off_with_no_rule_writes_nothing() {
        let (rec, c) = client(vec![list(vec![])]);
        block_on(c.autorespond_clear()).unwrap();
        assert_eq!(rec.kinds(), vec!["fauna.email.filters.list"]);
    }

    #[test]
    fn read_back_finds_the_rule() {
        let ours = filter(5, vec![], reply("Back Monday."));
        let (_, c) = client(vec![list(vec![
            filter(
                1,
                vec![sender_is("a@example.com")],
                EmailFilterAction::Discard,
            ),
            ours,
        ])]);
        assert_eq!(
            block_on(c.autorespond_get()).unwrap(),
            AutoRespond {
                enabled: true,
                message: "Back Monday.".into()
            }
        );
        let (_, c) = client(vec![list(vec![])]);
        assert_eq!(
            block_on(c.autorespond_get()).unwrap(),
            AutoRespond::default()
        );
    }

    #[test]
    fn a_hand_built_conditional_auto_reply_is_not_the_setting() {
        let user_rule = filter(1, vec![sender_is("boss@example.com")], reply("On leave"));
        assert_eq!(
            autorespond_from_filters(std::slice::from_ref(&user_rule)),
            AutoRespond::default()
        );
        // Turning the setting on adds the facade's own rule beside it.
        let (rec, c) = client(vec![list(vec![user_rule]), created()]);
        block_on(c.autorespond_set("Away")).unwrap();
        assert_eq!(rec.kinds()[1], "fauna.email.filters.create");
    }

    #[test]
    fn a_match_all_any_rule_is_not_the_setting() {
        // Zero conditions under `any` never match (the evaluator's vacuous
        // rule), so it is not an auto-respond rule.
        let mut never = filter(1, vec![], reply("x"));
        never.combination = "any".into();
        assert!(!is_autorespond_rule(&never));
    }

    #[test]
    fn a_hand_built_match_all_auto_reply_is_adopted_and_extras_collapse() {
        // Two match-all auto-replies built by raw API: the first in evaluation
        // order is read and kept (normalised); the other is deleted.
        let mut first = filter(8, vec![], reply("First"));
        first.priority = -5;
        let second = filter(3, vec![], reply("Second"));
        let filters = vec![second.clone(), first.clone()];
        assert_eq!(
            autorespond_from_filters(&filters),
            AutoRespond {
                enabled: true,
                message: "First".into()
            }
        );
        let (rec, c) = client(vec![list(filters), updated(), deleted()]);
        block_on(c.autorespond_set("Merged")).unwrap();
        assert_eq!(
            rec.kinds(),
            vec![
                "fauna.email.filters.list",
                "fauna.email.filters.update",
                "fauna.email.filters.delete"
            ]
        );
        let upd: UpdateEmailFilterRequest =
            fauna_protocol::decode_strict(&rec.payloads()[1]).unwrap();
        assert_eq!(upd.id, 8);
        assert!(upd.continue_on_match);
        assert_eq!(upd.priority, AUTORESPOND_PRIORITY);
        let del: DeleteEmailFilterRequest =
            fauna_protocol::decode_strict(&rec.payloads()[2]).unwrap();
        assert_eq!(del.id, 3);
    }
}
