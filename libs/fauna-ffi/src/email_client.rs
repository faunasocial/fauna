//! UniFFI façade for the `fauna.email.*` Layer-3 WS-RPC kinds — the
//! per-account email-filter CRUD plus outbound submission clients hit
//! from the mail / privacy settings UI.
//!
//! [`FfiEmailClient`] wraps `fauna_client_email::EmailClient` (which in
//! turn wraps the shared `NestClient`); the mirror enums/records below are
//! the FFI-visible shape of `fauna_protocol::email::*`. The Rust-native
//! Linux app (`apps/fauna-linux/src/settings/email_filters.rs`) calls
//! the same `EmailClient` directly — this seam gives Apple / Windows /
//! Android the identical surface over UniFFI.
//!
//! All conversions use exhaustive matches: adding a variant to the
//! protocol enums is a compile error here, so the mirror can't silently
//! drift (priority #1/#4).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_email::EmailClient;
use fauna_client_email::email::{
    EmailFilter, EmailFilterAction, EmailFilterRule, FilterActionInputs, SendEmailReply,
    describe_filter_action, describe_filter_rule, encode_filter_action, encode_filter_rule,
    filter_action_label, filter_is_editable_for,
};
use fauna_core::carried::CarriedValue;
use fauna_core::localized::LocalizedText;
use fauna_protocol::{Value, decode_strict, encode_canonical};

use crate::{FfiError, general_err, stringify};

// ── EmailFilterRule mirror ─────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::email::EmailFilterRule`].
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiEmailFilterRule {
    SenderIs {
        address: String,
    },
    SenderDomain {
        domain: String,
    },
    SubjectContains {
        text: String,
    },
    BodyContains {
        text: String,
    },
    HeaderExists {
        name: String,
    },
    HeaderContains {
        name: String,
        value: String,
    },
    /// Combined spam score (milli-int) `>= milli` — the "act on the spam score"
    /// condition (`smtp-server.md` § Email filter rules).
    SpamScoreAtLeast {
        milli: i32,
    },
    /// A condition a newer build added that this build cannot read, carried
    /// as its canonical dag-cbor so an edit echoes it unchanged. Never offered
    /// by a form; [`describe_email_filter_rule`] answers `None` for it.
    Unknown {
        cbor: Vec<u8>,
    },
}

/// The canonical bytes of a carried unknown variant, for the FFI mirror.
fn carried_to_ffi(v: &CarriedValue) -> Vec<u8> {
    // A value decoded by `decode_strict` always re-encodes.
    encode_canonical(v).map(|b| b.to_vec()).unwrap_or_default()
}

/// The carried unknown variant an app hands back. Only bytes this seam handed
/// out decode; anything else is refused rather than written to the nest.
fn carried_from_ffi(cbor: &[u8]) -> Result<CarriedValue, FfiError> {
    decode_strict(cbor).map_err(general_err)
}

/// For the read-only helpers (label, describe, editability) only the arm of a
/// carried unknown matters, never its value, so one whose bytes do not decode
/// still reads as unknown.
fn rule_for_reading(r: FfiEmailFilterRule) -> EmailFilterRule {
    r.try_into()
        .unwrap_or(EmailFilterRule::Unknown(CarriedValue(Value::Null)))
}

/// [`rule_for_reading`] for an action.
fn action_for_reading(a: FfiEmailFilterAction) -> EmailFilterAction {
    a.try_into()
        .unwrap_or(EmailFilterAction::Unknown(CarriedValue(Value::Null)))
}

impl TryFrom<FfiEmailFilterRule> for EmailFilterRule {
    type Error = FfiError;
    fn try_from(r: FfiEmailFilterRule) -> Result<Self, FfiError> {
        Ok(match r {
            FfiEmailFilterRule::SenderIs { address } => EmailFilterRule::SenderIs { address },
            FfiEmailFilterRule::SenderDomain { domain } => EmailFilterRule::SenderDomain { domain },
            FfiEmailFilterRule::SubjectContains { text } => {
                EmailFilterRule::SubjectContains { text }
            }
            FfiEmailFilterRule::BodyContains { text } => EmailFilterRule::BodyContains { text },
            FfiEmailFilterRule::HeaderExists { name } => EmailFilterRule::HeaderExists { name },
            FfiEmailFilterRule::HeaderContains { name, value } => {
                EmailFilterRule::HeaderContains { name, value }
            }
            FfiEmailFilterRule::SpamScoreAtLeast { milli } => {
                EmailFilterRule::SpamScoreAtLeast { milli }
            }
            FfiEmailFilterRule::Unknown { cbor } => {
                EmailFilterRule::Unknown(carried_from_ffi(&cbor)?)
            }
        })
    }
}

impl From<EmailFilterRule> for FfiEmailFilterRule {
    fn from(r: EmailFilterRule) -> Self {
        match r {
            EmailFilterRule::SenderIs { address } => FfiEmailFilterRule::SenderIs { address },
            EmailFilterRule::SenderDomain { domain } => FfiEmailFilterRule::SenderDomain { domain },
            EmailFilterRule::SubjectContains { text } => {
                FfiEmailFilterRule::SubjectContains { text }
            }
            EmailFilterRule::BodyContains { text } => FfiEmailFilterRule::BodyContains { text },
            EmailFilterRule::HeaderExists { name } => FfiEmailFilterRule::HeaderExists { name },
            EmailFilterRule::HeaderContains { name, value } => {
                FfiEmailFilterRule::HeaderContains { name, value }
            }
            EmailFilterRule::SpamScoreAtLeast { milli } => {
                FfiEmailFilterRule::SpamScoreAtLeast { milli }
            }
            EmailFilterRule::Unknown(v) => FfiEmailFilterRule::Unknown {
                cbor: carried_to_ffi(&v),
            },
        }
    }
}

// ── EmailFilterAction mirror ───────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::email::EmailFilterAction`].
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiEmailFilterAction {
    Allow,
    Discard,
    Reject {
        reason: String,
    },
    FileInto {
        mailbox: String,
    },
    /// `redirect`: the rule's copy mode — `false` keeps a local copy (the
    /// default), `true` forwards without local delivery. Mirrors the wire
    /// field 1:1 so the FFI seam never drops it on list → edit → save.
    Forward {
        address: String,
        redirect: bool,
    },
    AutoReply {
        subject: String,
        body: String,
        interval_hours: u32,
    },
    AddLabel {
        label: String,
    },
    /// An action a newer build added that this build cannot read, carried as
    /// its canonical dag-cbor so an edit echoes it unchanged. It does nothing;
    /// its label is the neutral one and no form opens on it.
    Unknown {
        cbor: Vec<u8>,
    },
}

impl TryFrom<FfiEmailFilterAction> for EmailFilterAction {
    type Error = FfiError;
    fn try_from(a: FfiEmailFilterAction) -> Result<Self, FfiError> {
        Ok(match a {
            FfiEmailFilterAction::Allow => EmailFilterAction::Allow,
            FfiEmailFilterAction::Discard => EmailFilterAction::Discard,
            FfiEmailFilterAction::Reject { reason } => EmailFilterAction::Reject { reason },
            FfiEmailFilterAction::FileInto { mailbox } => EmailFilterAction::FileInto { mailbox },
            FfiEmailFilterAction::Forward { address, redirect } => {
                EmailFilterAction::Forward { address, redirect }
            }
            FfiEmailFilterAction::AutoReply {
                subject,
                body,
                interval_hours,
            } => EmailFilterAction::AutoReply {
                subject,
                body,
                interval_hours,
            },
            FfiEmailFilterAction::AddLabel { label } => EmailFilterAction::AddLabel { label },
            FfiEmailFilterAction::Unknown { cbor } => {
                EmailFilterAction::Unknown(carried_from_ffi(&cbor)?)
            }
        })
    }
}

impl From<EmailFilterAction> for FfiEmailFilterAction {
    fn from(a: EmailFilterAction) -> Self {
        match a {
            EmailFilterAction::Allow => FfiEmailFilterAction::Allow,
            EmailFilterAction::Discard => FfiEmailFilterAction::Discard,
            EmailFilterAction::Reject { reason } => FfiEmailFilterAction::Reject { reason },
            EmailFilterAction::FileInto { mailbox } => FfiEmailFilterAction::FileInto { mailbox },
            EmailFilterAction::Forward { address, redirect } => {
                FfiEmailFilterAction::Forward { address, redirect }
            }
            EmailFilterAction::AutoReply {
                subject,
                body,
                interval_hours,
            } => FfiEmailFilterAction::AutoReply {
                subject,
                body,
                interval_hours,
            },
            EmailFilterAction::AddLabel { label } => FfiEmailFilterAction::AddLabel { label },
            EmailFilterAction::Unknown(v) => FfiEmailFilterAction::Unknown {
                cbor: carried_to_ffi(&v),
            },
        }
    }
}

// ── EmailFilter row mirror ─────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::email::EmailFilter`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiEmailFilter {
    pub id: i64,
    pub name: String,
    pub rules: Vec<FfiEmailFilterRule>,
    /// `"all"` or `"any"`.
    pub combination: String,
    pub action: FfiEmailFilterAction,
    pub priority: i32,
    /// Creation epoch in milliseconds.
    pub created_at: i64,
}

impl From<EmailFilter> for FfiEmailFilter {
    fn from(f: EmailFilter) -> Self {
        FfiEmailFilter {
            id: f.id,
            name: f.name,
            rules: f.rules.into_iter().map(Into::into).collect(),
            combination: f.combination,
            action: f.action.into(),
            priority: f.priority,
            created_at: f.created_at,
        }
    }
}

// ── SendEmailReply mirror ──────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::email::SendEmailReply`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSendEmailReply {
    pub local_delivered: u32,
    pub remote_queued: u32,
    pub remote_errors: Vec<String>,
}

impl From<SendEmailReply> for FfiSendEmailReply {
    fn from(r: SendEmailReply) -> Self {
        FfiSendEmailReply {
            local_delivered: r.local_delivered,
            remote_queued: r.remote_queued,
            remote_errors: r.remote_errors,
        }
    }
}

// ── FfiEmailClient ─────────────────────────────────────────────────────

/// UniFFI handle for the `fauna.email.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::email`]; methods are exposed to
/// Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiEmailClient {
    nest: Arc<NestClient>,
}

impl FfiEmailClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> EmailClient<Arc<NestClient>> {
        EmailClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiEmailClient {
    /// `fauna.email.filters.list`
    pub async fn filters_list(&self) -> Result<Vec<FfiEmailFilter>, FfiError> {
        let filters = self.client().filters_list().await.map_err(stringify)?;
        Ok(filters.into_iter().map(Into::into).collect())
    }

    /// `fauna.email.filters.create` — returns the new row id.
    pub async fn filters_create(
        &self,
        name: String,
        rules: Vec<FfiEmailFilterRule>,
        combination: String,
        action: FfiEmailFilterAction,
        priority: i32,
    ) -> Result<i64, FfiError> {
        let rules = rules
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<EmailFilterRule>, _>>()?;
        self.client()
            .filters_create(name, rules, combination, action.try_into()?, priority)
            .await
            .map_err(stringify)
    }

    /// `fauna.email.filters.get`
    pub async fn filters_get(&self, id: i64) -> Result<FfiEmailFilter, FfiError> {
        let filter = self.client().filters_get(id).await.map_err(stringify)?;
        Ok(filter.into())
    }

    /// `fauna.email.filters.update` — overwrite an existing rule.
    pub async fn filters_update(
        &self,
        id: i64,
        name: String,
        rules: Vec<FfiEmailFilterRule>,
        combination: String,
        action: FfiEmailFilterAction,
        priority: i32,
    ) -> Result<(), FfiError> {
        let rules = rules
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<EmailFilterRule>, _>>()?;
        self.client()
            .filters_update(id, name, rules, combination, action.try_into()?, priority)
            .await
            .map_err(stringify)
    }

    /// `fauna.email.filters.delete`
    pub async fn filters_delete(&self, id: i64) -> Result<(), FfiError> {
        self.client().filters_delete(id).await.map_err(stringify)
    }

    /// `fauna.email.send` — submit a raw RFC 5322 message. `forbid_replay`
    /// at 30 s: not auto-retried on disconnect (double-send risk).
    pub async fn send(
        &self,
        recipients: Vec<String>,
        raw_rfc5322: Vec<u8>,
    ) -> Result<FfiSendEmailReply, FfiError> {
        let reply = self
            .client()
            .send(recipients, raw_rfc5322)
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }
}

/// Build a typed [`FfiEmailFilterRule`] from the create-dialog `(kind, value)`
/// pair — the UniFFI face of [`fauna_protocol::email::encode_filter_rule`], so
/// Apple / Windows / Android share Linux's encoder instead of re-deriving the
/// dropdown→variant map per client (priority #2/#4). An unknown `kind` is an
/// `Err`, not a silent `SenderIs`.
#[uniffi::export]
pub fn encode_email_filter_rule(
    kind: String,
    value: String,
) -> Result<FfiEmailFilterRule, FfiError> {
    encode_filter_rule(&kind, &value)
        .map(Into::into)
        .map_err(general_err)
}

/// FFI mirror of [`fauna_protocol::email::FilterActionInputs`] — every input
/// the shared filter form collects for its action.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFilterActionInputs {
    /// One of `SUPPORTED_ACTION_KINDS` (`Allow`/`Discard`/`Reject`/`Forward`).
    pub kind: String,
    pub reject_reason: String,
    pub forward_address: String,
    /// The "keep a local copy" checkbox; unchecked is `redirect`.
    pub keep_local_copy: bool,
}

impl From<FilterActionInputs> for FfiFilterActionInputs {
    fn from(i: FilterActionInputs) -> Self {
        Self {
            kind: i.kind,
            reject_reason: i.reject_reason,
            forward_address: i.forward_address,
            keep_local_copy: i.keep_local_copy,
        }
    }
}

impl From<FfiFilterActionInputs> for FilterActionInputs {
    fn from(i: FfiFilterActionInputs) -> Self {
        Self {
            kind: i.kind,
            reject_reason: i.reject_reason,
            forward_address: i.forward_address,
            keep_local_copy: i.keep_local_copy,
        }
    }
}

/// Build a typed [`FfiEmailFilterAction`] from the shared filter form's action
/// inputs — the UniFFI face of [`fauna_protocol::email::encode_filter_action`].
/// A `Forward` destination is checked with the nest's own rule-path predicate;
/// an unknown tag or an invalid destination is `Err`.
#[uniffi::export]
pub fn encode_email_filter_action_inputs(
    inputs: FfiFilterActionInputs,
) -> Result<FfiEmailFilterAction, FfiError> {
    encode_filter_action(&inputs.into())
        .map(Into::into)
        .map_err(general_err)
}

/// The reverse of [`encode_email_filter_action_inputs`]: the form inputs that
/// reproduce a stored [`FfiEmailFilterAction`] (a `Forward`'s destination and
/// copy mode included), or `None` for the richer variants no form collects.
#[uniffi::export]
pub fn describe_email_filter_action_inputs(
    action: FfiEmailFilterAction,
) -> Option<FfiFilterActionInputs> {
    let action = action_for_reading(action);
    describe_filter_action(&action).map(Into::into)
}

/// Whether a stored filter can open in a form covering `action_kinds` — the
/// UniFFI face of [`fauna_protocol::email::filter_is_editable_for`]. A form
/// that collects the Forward inputs passes every supported kind.
#[uniffi::export]
pub fn email_filter_is_editable_for(
    rules: Vec<FfiEmailFilterRule>,
    action: FfiEmailFilterAction,
    action_kinds: Vec<String>,
) -> bool {
    let rules: Vec<EmailFilterRule> = rules.into_iter().map(rule_for_reading).collect();
    let action = action_for_reading(action);
    let kinds: Vec<&str> = action_kinds.iter().map(String::as_str).collect();
    filter_is_editable_for(&rules, &action, &kinds)
}

/// The `(kind, value)` pair [`fauna_protocol::email::describe_filter_rule`]
/// returns — the edit dialog's reverse of [`encode_email_filter_rule`].
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiDescribedFilterRule {
    pub kind: String,
    pub value: String,
}

/// The reverse of [`encode_email_filter_rule`]: the create-dialog `(kind,
/// value)` pair for a stored [`FfiEmailFilterRule`], so an edit form can
/// pre-populate the same dropdown + value field the create form uses.
/// `None` for the richer variants no dialog collects (`HeaderContains`,
/// `SpamScoreAtLeast`) — a decode gap here means "don't offer edit," not a
/// call failure.
#[uniffi::export]
pub fn describe_email_filter_rule(rule: FfiEmailFilterRule) -> Option<FfiDescribedFilterRule> {
    let rule = rule_for_reading(rule);
    describe_filter_rule(&rule).map(|(kind, value)| FfiDescribedFilterRule {
        kind: kind.to_string(),
        value,
    })
}

/// The `filter-action` list-row badge label for a stored
/// [`FfiEmailFilterAction`] — unlike [`describe_email_filter_action_inputs`] (`None`
/// for the variants no form collects, which only gates *editability*), this
/// always resolves. Lifts the per-app hand-rolled action → label map onto
/// one source of truth (priority #2/#4).
#[uniffi::export]
pub fn email_filter_action_label(action: FfiEmailFilterAction) -> LocalizedText {
    let action = action_for_reading(action);
    filter_action_label(&action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_round_trips_all_variants() {
        let variants = vec![
            FfiEmailFilterRule::SenderIs {
                address: "a@b.c".into(),
            },
            FfiEmailFilterRule::SenderDomain {
                domain: "b.c".into(),
            },
            FfiEmailFilterRule::SubjectContains { text: "hi".into() },
            FfiEmailFilterRule::BodyContains { text: "yo".into() },
            FfiEmailFilterRule::HeaderExists {
                name: "X-Foo".into(),
            },
            FfiEmailFilterRule::HeaderContains {
                name: "X-Foo".into(),
                value: "bar".into(),
            },
        ];
        for r in variants {
            let proto: EmailFilterRule = r.clone().try_into().unwrap();
            assert_eq!(FfiEmailFilterRule::from(proto), r);
        }
    }

    #[test]
    fn action_round_trips_all_variants() {
        let variants = vec![
            FfiEmailFilterAction::Allow,
            FfiEmailFilterAction::Discard,
            FfiEmailFilterAction::Reject {
                reason: "spam".into(),
            },
            FfiEmailFilterAction::FileInto {
                mailbox: "Archive".into(),
            },
            FfiEmailFilterAction::Forward {
                address: "bob@example.com".into(),
                redirect: true,
            },
            FfiEmailFilterAction::AutoReply {
                subject: "Out".into(),
                body: "Back Monday".into(),
                interval_hours: 12,
            },
            FfiEmailFilterAction::AddLabel {
                label: "important".into(),
            },
        ];
        for a in variants {
            let proto: EmailFilterAction = a.clone().try_into().unwrap();
            assert_eq!(FfiEmailFilterAction::from(proto), a);
        }
    }

    #[test]
    fn filter_row_maps() {
        let proto = EmailFilter {
            id: 42,
            name: "Move newsletters".into(),
            rules: vec![EmailFilterRule::SenderDomain {
                domain: "news.example".into(),
            }],
            combination: "any".into(),
            action: EmailFilterAction::FileInto {
                mailbox: "Reading".into(),
            },
            priority: 10,
            continue_on_match: false,
            created_at: 1_700_000_000_000,
            extra: Default::default(),
        };
        let ffi: FfiEmailFilter = proto.clone().into();
        assert_eq!(ffi.id, 42);
        assert_eq!(ffi.rules.len(), 1);
        assert_eq!(
            ffi.action,
            FfiEmailFilterAction::FileInto {
                mailbox: "Reading".into()
            }
        );
    }

    #[test]
    fn send_reply_maps() {
        let proto = SendEmailReply {
            local_delivered: 1,
            remote_queued: 2,
            remote_errors: vec!["bob@x: relay failed".into()],
            extra: Default::default(),
        };
        let ffi: FfiSendEmailReply = proto.into();
        assert_eq!(ffi.local_delivered, 1);
        assert_eq!(ffi.remote_queued, 2);
        assert_eq!(ffi.remote_errors.len(), 1);
    }

    #[test]
    fn encode_email_filter_rule_export_maps_to_ffi_variant() {
        assert_eq!(
            encode_email_filter_rule("SenderDomain".into(), "news.example".into()).unwrap(),
            FfiEmailFilterRule::SenderDomain {
                domain: "news.example".into()
            }
        );
        assert_eq!(
            encode_email_filter_rule("HeaderExists".into(), "List-Id".into()).unwrap(),
            FfiEmailFilterRule::HeaderExists {
                name: "List-Id".into()
            }
        );
        // Unknown kind surfaces as an FfiError rather than a silent rule.
        assert!(encode_email_filter_rule("sender".into(), "x".into()).is_err());
    }

    /// The inputs of a form that collects only a tag: every other input at its
    /// default (empty reason, no destination, copy box checked).
    fn tag_inputs(kind: &str) -> FfiFilterActionInputs {
        FilterActionInputs::new(kind.to_string()).into()
    }

    /// Every supported kind, as a form that collects the Forward inputs passes it.
    fn all_kinds() -> Vec<String> {
        fauna_client_email::email::SUPPORTED_ACTION_KINDS
            .iter()
            .map(|k| k.to_string())
            .collect()
    }

    #[test]
    fn encode_email_filter_action_inputs_export_defaults_reject_reason() {
        assert_eq!(
            encode_email_filter_action_inputs(tag_inputs("Reject")).unwrap(),
            FfiEmailFilterAction::Reject {
                reason: "Rejected by filter".into()
            }
        );
        assert_eq!(
            encode_email_filter_action_inputs(tag_inputs("Discard")).unwrap(),
            FfiEmailFilterAction::Discard
        );
        assert!(encode_email_filter_action_inputs(tag_inputs("discard")).is_err());
    }

    #[test]
    fn describe_email_filter_rule_export_is_the_exact_inverse_of_encode() {
        let rule = encode_email_filter_rule("SenderDomain".into(), "news.example".into()).unwrap();
        assert_eq!(
            describe_email_filter_rule(rule),
            Some(FfiDescribedFilterRule {
                kind: "SenderDomain".into(),
                value: "news.example".into(),
            })
        );
    }

    #[test]
    fn describe_email_filter_rule_export_is_none_for_kinds_no_dialog_collects() {
        assert_eq!(
            describe_email_filter_rule(FfiEmailFilterRule::SpamScoreAtLeast { milli: 500 }),
            None
        );
    }

    #[test]
    fn describe_email_filter_action_inputs_export_is_the_exact_inverse_of_encode() {
        let action = encode_email_filter_action_inputs(tag_inputs("Allow")).unwrap();
        assert_eq!(
            describe_email_filter_action_inputs(action),
            Some(tag_inputs("Allow"))
        );
    }

    #[test]
    fn describe_email_filter_action_inputs_export_is_none_for_kinds_no_dialog_collects() {
        assert_eq!(
            describe_email_filter_action_inputs(FfiEmailFilterAction::AddLabel {
                label: "Newsletters".into()
            }),
            None
        );
    }

    #[test]
    fn email_filter_is_editable_for_export_true_for_single_dialog_covered_rule_and_action() {
        assert!(email_filter_is_editable_for(
            vec![FfiEmailFilterRule::SenderIs {
                address: "a@b.example".into()
            }],
            FfiEmailFilterAction::Allow,
            all_kinds(),
        ));
    }

    #[test]
    fn email_filter_is_editable_for_export_false_for_multi_rule_or_uncovered_shapes() {
        assert!(!email_filter_is_editable_for(
            vec![
                FfiEmailFilterRule::SenderIs {
                    address: "a@b.example".into()
                },
                FfiEmailFilterRule::SenderDomain {
                    domain: "b.example".into()
                },
            ],
            FfiEmailFilterAction::Allow,
            all_kinds(),
        ));
        assert!(!email_filter_is_editable_for(
            vec![FfiEmailFilterRule::HeaderContains {
                name: "X-Spam".into(),
                value: "yes".into(),
            }],
            FfiEmailFilterAction::Allow,
            all_kinds(),
        ));
        assert!(!email_filter_is_editable_for(
            vec![FfiEmailFilterRule::SenderIs {
                address: "a@b.example".into()
            }],
            FfiEmailFilterAction::FileInto {
                mailbox: "Reading".into()
            },
            all_kinds(),
        ));
    }

    /// The inputs exports round-trip a `redirect` Forward losslessly and open
    /// it in a form that covers every supported kind.
    #[test]
    fn inputs_exports_round_trip_a_redirect_forward() {
        let forward = FfiEmailFilterAction::Forward {
            address: "bob@example.net".into(),
            redirect: true,
        };
        let inputs = describe_email_filter_action_inputs(forward.clone()).expect("describable");
        assert!(!inputs.keep_local_copy);
        assert_eq!(encode_email_filter_action_inputs(inputs).unwrap(), forward);
        assert!(email_filter_is_editable_for(
            vec![FfiEmailFilterRule::SenderIs {
                address: "a@b.example".into()
            }],
            forward,
            all_kinds(),
        ));
    }

    /// A rule and an action a newer build wrote cross the FFI seam as their
    /// canonical bytes and come back to the identical protocol value, so an app
    /// that lists a filter and saves it echoes what it could not read; the
    /// read-only helpers show it neutral and refuse to open it in a form; and
    /// bytes this seam never handed out are refused on the write path.
    #[test]
    fn unknown_rule_and_action_cross_the_ffi_seam_unchanged() {
        use std::collections::BTreeMap;
        let rule = EmailFilterRule::Unknown(CarriedValue(Value::Map(BTreeMap::from([(
            "ListIdIs".to_string(),
            Value::Map(BTreeMap::from([(
                "list_id".to_string(),
                Value::String("dev.example.org".into()),
            )])),
        )]))));
        let action = EmailFilterAction::Unknown(CarriedValue(Value::String("Quarantine".into())));

        let ffi_rule = FfiEmailFilterRule::from(rule.clone());
        let ffi_action = FfiEmailFilterAction::from(action.clone());
        assert!(matches!(ffi_rule, FfiEmailFilterRule::Unknown { .. }));
        assert_eq!(EmailFilterRule::try_from(ffi_rule.clone()).unwrap(), rule);
        assert_eq!(
            EmailFilterAction::try_from(ffi_action.clone()).unwrap(),
            action
        );

        assert_eq!(describe_email_filter_rule(ffi_rule.clone()), None);
        assert_eq!(
            describe_email_filter_action_inputs(ffi_action.clone()),
            None
        );
        assert_eq!(
            email_filter_action_label(ffi_action.clone()),
            LocalizedText::key("common.unknown")
        );
        assert!(!email_filter_is_editable_for(
            vec![ffi_rule],
            ffi_action,
            all_kinds()
        ));

        let forged = FfiEmailFilterAction::Unknown {
            cbor: vec![0xff, 0x00],
        };
        assert!(EmailFilterAction::try_from(forged.clone()).is_err());
        assert_eq!(
            email_filter_action_label(forged),
            LocalizedText::key("common.unknown")
        );
    }
}
