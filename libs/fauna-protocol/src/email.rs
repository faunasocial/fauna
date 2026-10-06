//! User-facing WS-RPC payload types for the Layer-3 mail-server adjacent
//! surface — what end-user clients call from the per-account mail UI:
//! the sieve-like per-account filter-rule engine (`fauna.email.filters.*`)
//! plus outbound submission (`fauna.email.send`).
//!
//! Distinct namespace from `bridges_ui.rs` — the URL hierarchy was
//! `/api/v1/email/*` and the goal-doc home is `docs/goal/behavior/
//! smtp-server.md` + `mail-content-scanning.md`, not `bridges.md`.
//! The kinds live under `fauna.email.*` (separate top namespace, matches URL
//! hierarchy + `features/mail-*` goal-doc grouping).
//!
//! Kind registry entries live in `kind.rs::register_email_kinds`.
//!
//! [`EmailFilterRule`] is the **canonical storage shape** and the sole rule
//! type: the nest CRUD handler serializes `Vec<EmailFilterRule>` straight into
//! the `email_filters.rules` BLOB (canonical dag-cbor), so this type owns the
//! on-disk rule format. The Go MTA perimeter re-decodes those bytes into
//! `fauna_mail::filter::FilterCondition` to evaluate (`smtp-server.md` §
//! Email filter rules). [`EmailFilterAction`] projects to/from the on-disk
//! `email_filters.action` string via nest's `email_handlers::action_to_string`
//! / `action_from_string` at the handler boundary — plus, for `Forward`, the
//! additive `email_filters.forward_redirect` column that carries the copy
//! mode (a column, not a new action-string prefix, so the action string stays
//! unambiguous).

use crate::Value;
use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── EmailFilterRule (wire + storage) ───────────────────────────────
//
// Single match criterion for inbound email, evaluated at the MTA perimeter
// (`fauna_mail::filter`). This is also the on-disk rule shape (serialized into
// `email_filters.rules` as canonical dag-cbor — Layer-6 Domain E). dag-cbor
// keys enum variants by NAME, so *renaming* a variant is the compat-breaking
// change (reordering is now safe); the Go MTA-eval mirror
// `fauna_mail::filter::FilterCondition` must keep matching variant names for
// the shared subset it decodes.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EmailFilterRule {
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
    /// The combined spam score (milli-int, per `fauna_mail::spam`) is
    /// `>= milli`. The "user-defined algorithm acts on the spam score"
    /// condition (`smtp-server.md` § Spam handling step 5 / § Email filter
    /// rules). Scaled int — no float crosses the dag-cbor wire.
    SpamScoreAtLeast {
        milli: i32,
    },
    /// A condition a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// carrying). Carried whole so an edit echoes it unchanged; it never
    /// matches, and no app offers it.
    #[serde(untagged)]
    Unknown(fauna_core::carried::CarriedValue),
}

// ── EmailFilterAction (wire) ───────────────────────────────────────
//
// Action to take when a filter matches. The sole action type; nest's
// `email_handlers` projects it to/from the on-disk `email_filters.action`
// string at the handler boundary.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EmailFilterAction {
    Allow,
    Discard,
    Reject {
        reason: String,
    },
    FileInto {
        mailbox: String,
    },
    /// Forward the message to `address` (`mail-forwarding.md` § Per-rule
    /// "forward to"). `redirect` is the rule's copy mode: `false` (the
    /// default — `copy`, keep the local copy and forward) or `true`
    /// (`redirect`, forward with no local delivery). Additive, `#[serde(default)]`:
    /// an absent key reads as `copy`, the safe default. It is NOT
    /// preserved through such a peer (no per-variant `extra` catch-all). An
    /// app keeps it through an edit because [`describe_filter_action`] hands
    /// the form the copy mode and [`encode_filter_action`] writes it back; a
    /// form whose `action_kinds` omit `Forward` never opens one at all
    /// ([`filter_is_editable_for`]).
    Forward {
        address: String,
        #[serde(default)]
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
    /// An action a newer build added that this build cannot read
    /// (`transport.md` § Schema and forward-compat discipline, rule 3: open,
    /// carrying). Carried whole so an edit echoes it unchanged; a filter whose
    /// action is unknown does nothing — never `Discard` — and no app offers it.
    #[serde(untagged)]
    Unknown(fauna_core::carried::CarriedValue),
}

// ── UI dropdown → typed wire encoders ──────────────────────────────
//
// The create-email-filter dialog on every app maps a rule-kind tag +
// free-text value (and an action tag) onto the typed [`EmailFilterRule`] /
// [`EmailFilterAction`] variants. That mapping was hand-rolled in all five
// app UIs and had diverged on tag casing, rule coverage, reject-reason
// defaults, and unknown-kind handling (priority #1/#3/#4). These functions are
// the single source of truth — the mail analog of the feed
// `encode_filter_rule` (`fauna_ffi::feed_client`). The Rust-native Linux app
// calls them directly; `fauna-ffi` / `fauna-wasm` wrap them for the other
// apps. Living beside the wire enums (the wasm-safe protocol base) keeps the
// emitted shape from drifting from what the nest deserializes.

/// Canonical SMTP reason substituted into a `Reject` action when the create
/// dialog collects none (no client UI collects one yet — they all hardcoded a
/// default that had drifted three ways).
pub const DEFAULT_REJECT_REASON: &str = "Rejected by filter";

/// A create-dialog kind/action tag that doesn't map to a known variant. The
/// pre-lift clients silently coerced this (to `SenderIs` / `Allow`, or — on web
/// — an empty rule); the shared encoders surface it so the UI can report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFilterKind(pub String);

impl std::fmt::Display for UnknownFilterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown email filter kind: {}", self.0)
    }
}

impl std::error::Error for UnknownFilterKind {}

/// The rule-kind tags [`encode_filter_rule`] accepts, in the order every
/// app's create-dialog dropdown presents them (linux and tui each hand-copied
/// this exact set as their own dropdown-options array before this constant
/// existed — priority #2/#3).
pub const SUPPORTED_RULE_KINDS: &[&str] = &[
    "SenderIs",
    "SenderDomain",
    "SubjectContains",
    "BodyContains",
    "HeaderExists",
];

/// The action tags [`encode_filter_action`] accepts, in dropdown order.
pub const SUPPORTED_ACTION_KINDS: &[&str] = &["Allow", "Discard", "Reject", "Forward"];

/// Build the typed [`EmailFilterRule`] for a create-dialog `(kind, value)` pair.
///
/// `kind` is the PascalCase variant name (the canonical tag — `"SenderIs"`,
/// `"SenderDomain"`, `"SubjectContains"`, `"BodyContains"`, `"HeaderExists"`);
/// `value` is the single free-text field (an address / domain / substring /
/// header name). Covers the single-string-value conditions the create UI offers
/// (`smtp-server.md` § Email filter rules). The multi-field / non-string
/// conditions (`HeaderContains`, `SpamScoreAtLeast`) are deferred until a UI
/// collects their extra inputs — they map to `Err` here, like any unknown kind.
pub fn encode_filter_rule(kind: &str, value: &str) -> Result<EmailFilterRule, UnknownFilterKind> {
    Ok(match kind {
        "SenderIs" => EmailFilterRule::SenderIs {
            address: value.to_string(),
        },
        "SenderDomain" => EmailFilterRule::SenderDomain {
            domain: value.to_string(),
        },
        "SubjectContains" => EmailFilterRule::SubjectContains {
            text: value.to_string(),
        },
        "BodyContains" => EmailFilterRule::BodyContains {
            text: value.to_string(),
        },
        "HeaderExists" => EmailFilterRule::HeaderExists {
            name: value.to_string(),
        },
        other => return Err(UnknownFilterKind(other.to_string())),
    })
}

/// Every input the shared filter form collects for its action — what
/// [`encode_filter_action`] reads and [`describe_filter_action`] returns. One
/// typed struct rather than a positional argument per input, so each action
/// kind a form learns to collect (file-into's mailbox, add-label's label,
/// auto-reply's subject/body/interval) is one more field here, not another
/// encoder signature on all seven apps. Serde-shaped (snake_case, every field
/// defaulted) so the web form passes it across the wasm boundary as a plain
/// object; it is a form value, never a wire payload — so it is version-locked
/// (encoder and form ship in one build) and refuses an unknown key rather than
/// carrying the rule-4 `extra` catch-all: a misspelled input fails loudly
/// instead of silently falling back to its default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilterActionInputs {
    /// The action tag — one of [`SUPPORTED_ACTION_KINDS`].
    pub kind: String,
    /// `Reject`'s SMTP reason; empty falls back to [`DEFAULT_REJECT_REASON`].
    pub reject_reason: String,
    /// `Forward`'s destination address (`mail-forwarding.md` § Per-rule
    /// "forward to" — the Destination text field).
    pub forward_address: String,
    /// `Forward`'s copy mode as the form shows it: the "keep a local copy"
    /// checkbox, checked by default (`copy`); unchecked is `redirect`.
    pub keep_local_copy: bool,
}

impl FilterActionInputs {
    /// The form's initial state for action `kind`: every other input at its
    /// default (no reject reason, no destination, keep-a-local-copy checked).
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            reject_reason: String::new(),
            forward_address: String::new(),
            keep_local_copy: true,
        }
    }
}

impl Default for FilterActionInputs {
    /// A fresh form: the first dropdown entry with every input at its default.
    fn default() -> Self {
        Self::new(SUPPORTED_ACTION_KINDS[0])
    }
}

/// Why the shared filter form's action inputs don't encode (see
/// [`encode_filter_action`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterActionError {
    /// The action tag isn't one of [`SUPPORTED_ACTION_KINDS`].
    UnknownKind(String),
    /// A `Forward` whose destination fails [`validate_forward_target`].
    ForwardAddress(ForwardTargetError),
}

impl std::fmt::Display for FilterActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilterActionError::UnknownKind(kind) => {
                write!(f, "unknown email filter kind: {kind}")
            }
            FilterActionError::ForwardAddress(e) => write!(f, "forward address: {e}"),
        }
    }
}

impl std::error::Error for FilterActionError {}

/// Build the typed [`EmailFilterAction`] from the shared filter form's action
/// inputs.
///
/// `inputs.kind` is the PascalCase variant name (`"Allow"`, `"Discard"`,
/// `"Reject"`, `"Forward"`). A `Reject` takes `reject_reason`, an empty one
/// falling back to [`DEFAULT_REJECT_REASON`]. A `Forward` takes the trimmed
/// `forward_address`, checked with [`validate_forward_target`] against no
/// hosted-domain list — the same check the nest applies to a rule's
/// destination (only forward-all is barred from a domain we host) — and
/// `keep_local_copy` (unchecked ⇒ `redirect: true`). The richer actions no form
/// collects yet (`FileInto`, `AutoReply`, `AddLabel`) map to `UnknownKind`.
pub fn encode_filter_action(
    inputs: &FilterActionInputs,
) -> Result<EmailFilterAction, FilterActionError> {
    Ok(match inputs.kind.as_str() {
        "Allow" => EmailFilterAction::Allow,
        "Discard" => EmailFilterAction::Discard,
        "Reject" => EmailFilterAction::Reject {
            reason: if inputs.reject_reason.is_empty() {
                DEFAULT_REJECT_REASON.to_string()
            } else {
                inputs.reject_reason.clone()
            },
        },
        "Forward" => {
            let address = inputs.forward_address.trim();
            validate_forward_target(address, &[]).map_err(FilterActionError::ForwardAddress)?;
            EmailFilterAction::Forward {
                address: address.to_string(),
                redirect: !inputs.keep_local_copy,
            }
        }
        other => return Err(FilterActionError::UnknownKind(other.to_string())),
    })
}

/// Reverse of [`encode_filter_rule`] — the create-dialog `(kind, value)` pair
/// that reproduces a stored [`EmailFilterRule`], for populating an edit form.
/// `None` for the richer variants no dialog collects (`HeaderContains`,
/// `SpamScoreAtLeast`) — the same boundary `encode_filter_rule` enforces on
/// write, held on read too, so an edit form never silently narrows a rule it
/// can't fully represent.
pub fn describe_filter_rule(rule: &EmailFilterRule) -> Option<(&'static str, String)> {
    Some(match rule {
        EmailFilterRule::SenderIs { address } => ("SenderIs", address.clone()),
        EmailFilterRule::SenderDomain { domain } => ("SenderDomain", domain.clone()),
        EmailFilterRule::SubjectContains { text } => ("SubjectContains", text.clone()),
        EmailFilterRule::BodyContains { text } => ("BodyContains", text.clone()),
        EmailFilterRule::HeaderExists { name } => ("HeaderExists", name.clone()),
        EmailFilterRule::HeaderContains { .. }
        | EmailFilterRule::SpamScoreAtLeast { .. }
        | EmailFilterRule::Unknown(_) => {
            return None;
        }
    })
}

/// Reverse of [`encode_filter_action`] — the form inputs that reproduce a
/// stored [`EmailFilterAction`], for populating an edit form: a `Reject`
/// carries its reason and a `Forward` its destination and copy mode
/// (`redirect` ⇒ keep-a-local-copy unchecked), so a form that holds every
/// field saves the action back unchanged. `None` for the richer variants no
/// form collects (`FileInto`, `AutoReply`, `AddLabel`), same boundary as
/// `encode_filter_action`.
pub fn describe_filter_action(action: &EmailFilterAction) -> Option<FilterActionInputs> {
    Some(match action {
        EmailFilterAction::Allow => FilterActionInputs::new("Allow"),
        EmailFilterAction::Discard => FilterActionInputs::new("Discard"),
        EmailFilterAction::Reject { reason } => FilterActionInputs {
            reject_reason: reason.clone(),
            ..FilterActionInputs::new("Reject")
        },
        EmailFilterAction::Forward { address, redirect } => FilterActionInputs {
            forward_address: address.clone(),
            keep_local_copy: !redirect,
            ..FilterActionInputs::new("Forward")
        },
        EmailFilterAction::FileInto { .. }
        | EmailFilterAction::AutoReply { .. }
        | EmailFilterAction::AddLabel { .. }
        | EmailFilterAction::Unknown(_) => return None,
    })
}

/// The `filter-action` list-row badge label for a stored [`EmailFilterAction`]
/// — unlike [`describe_filter_action`] (`None` for the richer variants no form
/// collects, which only gates *editability*, not display), this always resolves, as a
/// [`LocalizedText`] each app resolves through its own i18n pipeline.
///
/// Shared so the label decision can't drift per-app (priority #2/#4): all
/// seven apps hand-rolled this independently and had diverged — linux/tui
/// rendered the bare wire-variant name (`"FileInto"`/`"AutoReply"`/
/// `"AddLabel"`), windows fell back to raw snake_case tokens for the same
/// four (`"file_into"`/`"auto_reply"`/`"add_label"`), web hardcoded the
/// literal `"Reject"` for every struct-variant action regardless of which one
/// it actually was, and only apple had a correct, complete label for all
/// seven. This converges every app on apple's canonical short labels (a new
/// `status.email_filters.action_*` key each for the four richer variants;
/// `Allow`/`Discard`/`Reject` reuse the existing keys). See `filter-action`'s
/// ui.yaml description ("Filter action display (Allow/Discard/Reject)").
pub fn filter_action_label(action: &EmailFilterAction) -> LocalizedText {
    match action {
        EmailFilterAction::Allow => LocalizedText::key("status.email_filters.action_allow"),
        EmailFilterAction::Discard => LocalizedText::key("status.email_filters.action_discard"),
        EmailFilterAction::Reject { .. } => {
            LocalizedText::key("status.email_filters.action_reject")
        }
        EmailFilterAction::FileInto { .. } => {
            LocalizedText::key("status.email_filters.action_file_into")
        }
        EmailFilterAction::Forward { .. } => {
            LocalizedText::key("status.email_filters.action_forward")
        }
        EmailFilterAction::AutoReply { .. } => {
            LocalizedText::key("status.email_filters.action_auto_reply")
        }
        EmailFilterAction::AddLabel { .. } => {
            LocalizedText::key("status.email_filters.action_add_label")
        }
        // An action a newer build added: shown neutral, never as a known one.
        EmailFilterAction::Unknown(_) => LocalizedText::key("common.unknown"),
    }
}

/// Whether a stored filter can be opened in the shared edit dialog: exactly
/// one rule, and both the rule and the action fall in the create-dialog's
/// dropdown-covered subset (`describe_filter_rule` / `describe_filter_action`
/// both `Some`). Clients gate their per-row edit affordance on this so a
/// filter only a raw API call could have produced (multi-rule, or a richer
/// rule/action) never opens a form that would silently narrow it on save.
pub fn filter_is_editable(filter: &EmailFilter) -> bool {
    filter_is_editable_for(&filter.rules, &filter.action, SUPPORTED_ACTION_KINDS)
}

/// [`filter_is_editable`] over a stored filter's `rules` + `action`, for a form
/// that covers only `action_kinds` — a form that lacks an action kind's inputs
/// leaves that kind out, so it never opens a stored rule it would narrow on
/// save.
pub fn filter_is_editable_for(
    rules: &[EmailFilterRule],
    action: &EmailFilterAction,
    action_kinds: &[&str],
) -> bool {
    rules.len() == 1
        && describe_filter_rule(&rules[0]).is_some()
        && describe_filter_action(action)
            .is_some_and(|inputs| action_kinds.contains(&inputs.kind.as_str()))
}

// ── AddLabel keyword validation ────────────────────────────────────

/// Why an `AddLabel { label }` value is not a valid IMAP keyword
/// (see [`validate_label`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidLabel {
    /// Empty — a keyword is `1*ATOM-CHAR`, at least one char.
    Empty,
    /// Starts with `\` — the IMAP system-flag namespace (`\Deleted`, `\Seen`, …),
    /// which a user label must never name. Reported distinctly from a generic
    /// illegal char so the create-time error is self-explanatory.
    SystemFlag,
    /// Contains an RFC 3501 `atom-special`, whitespace, control, or non-ASCII
    /// byte (the offending char). A keyword is a single bare `atom`.
    IllegalChar(char),
}

impl std::fmt::Display for InvalidLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvalidLabel::Empty => write!(f, "must not be empty"),
            InvalidLabel::SystemFlag => {
                write!(f, "must not be an IMAP system flag (no leading backslash)")
            }
            InvalidLabel::IllegalChar(c) => write!(
                f,
                "contains an illegal character {c:?} (must be a single IMAP keyword)"
            ),
        }
    }
}

impl std::error::Error for InvalidLabel {}

/// RFC 3501 `ATOM-CHAR` — any `CHAR` (%x01-7F) except an `atom-special`
/// (`( ) { SP CTL list-wildcards quoted-specials resp-specials`). Non-ASCII
/// (>%x7F) is not a `CHAR`, so it is excluded too.
fn is_atom_char(c: char) -> bool {
    if !c.is_ascii() {
        return false;
    }
    let b = c as u8;
    if b < 0x20 || b == 0x7f {
        return false; // CTL
    }
    // atom-special = "(" / ")" / "{" / SP / CTL / list-wildcards ("%" "*") /
    //                quoted-specials (DQUOTE "\") / resp-specials ("]")
    !matches!(
        b,
        b'(' | b')' | b'{' | b' ' | b'%' | b'*' | b'"' | b'\\' | b']'
    )
}

/// Validate an `AddLabel { label }` value as a single RFC 3501 IMAP keyword
/// (a `flag-keyword`, i.e. an `atom`).
///
/// At inbound delivery a matched `AddLabel` rides the message as an IMAP keyword
/// flag (`smtp-server.md` § Email filter rules: "`AddLabel` accumulates — every
/// matched label rides the delivery as an IMAP keyword"). Stored flags are
/// space-separated and `split_whitespace()`-scanned by the nest ingest +
/// IMAP EXPUNGE/unseen paths, where a **system flag** (`\Deleted` →
/// EXPUNGE-eligible, `\Seen` → silently read) or a **whitespace-splittable**
/// value would be honoured as several flags. So a label must be exactly one
/// keyword `atom`: non-empty, no leading `\` (the system-flag namespace), and
/// no `atom-special` / SP / control / non-ASCII byte.
///
/// Shared (wasm-safe) so the nest create-time handler (`validate_action`) and a
/// future client create-filter dialog enforce one rule (priority #2); the nest
/// also re-screens labels with this predicate at ingest as defense-in-depth
/// (`bridge_routing_handlers`).
pub fn validate_label(label: &str) -> Result<(), InvalidLabel> {
    if label.is_empty() {
        return Err(InvalidLabel::Empty);
    }
    if label.starts_with('\\') {
        return Err(InvalidLabel::SystemFlag);
    }
    if let Some(c) = label.chars().find(|&c| !is_atom_char(c)) {
        return Err(InvalidLabel::IllegalChar(c));
    }
    Ok(())
}

// ── FileInto target validation ─────────────────────────────────────

/// The ward's **held mailbox** (`family-safety.md` § The mail gate): where cold
/// inbound mail lands while `unknown_sender_mail = hold` and the guardian has
/// not yet reviewed the sender.
///
/// Deliberately **not** `Junk` and deliberately **not** a seventh standard
/// mailbox:
///
/// - Not `Junk`, because the ward must be able to tell *"your guardian is
///   reviewing this"* from *"this looked like spam"* (§ The trust shape
///   invariant 4 — supervision is transparent), and because the MDA's
///   SELECT-time per-user spam scorer independently re-files INBOX→Junk
///   (`mail-spam.md`) and would race a release.
/// - Not one of the six standard mailboxes, because those are seeded for
///   **every** actor on first AUTH and carry RFC 6154 SPECIAL-USE attributes,
///   of which there is none for "held" (`imap-server.md` § Standard mailboxes:
///   "special-use attributes other than the six above are not assigned"). This
///   mailbox is auto-created on the first hold, so an unsupervised account — and
///   a ward whose guardian never chose `hold` — never sees it.
///
/// The name is what the ward reads in their own MUA, so it says who is holding
/// the mail and why. It lives here, beside [`validate_file_into_mailbox`] (which
/// refuses it), so the nest's mail store and the filter check share one
/// spelling.
pub const GUARDIAN_HELD_MAILBOX: &str = "Guardian Review";

/// Why a `FileInto { mailbox }` target may never receive inbound mail
/// (see [`validate_file_into_mailbox`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidFileIntoMailbox {
    /// `Sent` — it holds only mail this account sent.
    Sent,
    /// `Drafts` — it holds only this account's own unsent compositions.
    Drafts,
    /// [`GUARDIAN_HELD_MAILBOX`] — a message there is a hold.
    GuardianHeld,
}

impl std::fmt::Display for InvalidFileIntoMailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvalidFileIntoMailbox::Sent => {
                write!(f, "must not be Sent: it holds only mail this account sent")
            }
            InvalidFileIntoMailbox::Drafts => write!(
                f,
                "must not be Drafts: it holds only this account's own drafts"
            ),
            InvalidFileIntoMailbox::GuardianHeld => write!(
                f,
                "must not be the guardian's held mailbox: only a hold places mail there"
            ),
        }
    }
}

impl std::error::Error for InvalidFileIntoMailbox {}

/// Validate a `FileInto { mailbox }` target — the mailbox a matched rule files
/// inbound mail into (`email-filters.md` § Email filter rules).
///
/// A rule may file into any mailbox inbound mail can belong in (`INBOX`,
/// `Archive`, `Junk`, `Trash`, a custom folder), but never into one whose
/// contents claim something an inbound delivery would forge:
///
/// - `Sent` holds only submissions this account authenticated. The SMTP rail
///   reads a `Sent` record as the user's own send and hides mark-as-spam for
///   it, so a stranger's message filed there would render as the user's own.
/// - `Drafts` holds the account's own unsent compositions, which a MUA reopens
///   and sends under the account's identity.
/// - [`GUARDIAN_HELD_MAILBOX`] belongs to the hold path: a message lands there
///   together with its hold record, and the name is reserved until a first
///   hold creates the mailbox.
///
/// Names match exactly, as the mailbox store does: `sent` is an ordinary custom
/// folder that no feed reads as `Sent`. Mailbox-name syntax is not this
/// predicate's; the nest checks it alongside.
///
/// Shared (wasm-safe) so the nest create-time handler (`validate_action`) and a
/// future client create-filter dialog enforce one rule; the nest also
/// re-screens the target with this predicate at placement as defense-in-depth
/// (`bridge_routing_handlers`), falling back to the spam disposition.
pub fn validate_file_into_mailbox(mailbox: &str) -> Result<(), InvalidFileIntoMailbox> {
    match mailbox {
        "Sent" => Err(InvalidFileIntoMailbox::Sent),
        "Drafts" => Err(InvalidFileIntoMailbox::Drafts),
        GUARDIAN_HELD_MAILBOX => Err(InvalidFileIntoMailbox::GuardianHeld),
        _ => Ok(()),
    }
}

// ── Forward target validation ──────────────────────────────────────
//
// The RFC 5321 syntactic check behind `mail-forwarding.md:31` ("the client
// validates the address (RFC 5321 syntactic check), and nest re-validates
// server-side") and `:244` ("Don't allow forward-all to point at a domain we
// host"). A **syntactic** check only: "wire-time validation only catches
// typos; the actual deliverability of the address is empirically discovered on
// first forward." Following `smtp-server.md:18` (choose the stricter option) it
// rejects the typo-shaped cases — embedded whitespace, a dotless domain, an
// over-long path — that a permissive RFC reading would let through. Lives here,
// beside the wire enums, so the shared filter-form encoder, the nest's
// create/update handlers and the forward-all knob (via its `fauna_mail`
// re-export) share one predicate.

/// RFC 5321 §4.5.3.1.1/.2 ceilings.
const MAX_LOCAL_PART_LEN: usize = 64;
const MAX_DOMAIN_LEN: usize = 255;
/// RFC 5321 §4.5.3.1.3 reverse/forward-path total length.
const MAX_PATH_LEN: usize = 254;
/// Why a forward-target address was rejected at validation time.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForwardTargetError {
    #[error("address is empty")]
    Empty,
    #[error("address is missing an `@`")]
    NoAt,
    #[error("address has more than one `@`")]
    MultipleAt,
    #[error("address exceeds the RFC 5321 length limit")]
    TooLong,
    #[error("local-part is empty or contains invalid characters")]
    LocalPartInvalid,
    #[error("domain is empty, dotless, or contains invalid characters")]
    DomainInvalid,
    /// The target is a domain this deployment hosts — the user should add an
    /// alias, not configure a forward (`mail-forwarding.md:244`).
    #[error("forward target is a local domain; add an alias instead")]
    IsLocalDomain,
}
/// Validate a forward-all / forward-rule target address (`mail-forwarding.md:31`)
/// and reject targets on a domain we host (`:244`). `local_domains` are this
/// deployment's hosted domains (matched case-insensitively); a per-rule
/// destination passes none — only forward-all is barred from a hosted domain
/// (the nest's rule path and [`encode_filter_action`] both pass `&[]`).
pub fn validate_forward_target(
    address: &str,
    local_domains: &[&str],
) -> Result<(), ForwardTargetError> {
    if address.is_empty() {
        return Err(ForwardTargetError::Empty);
    }
    if address.len() > MAX_PATH_LEN {
        return Err(ForwardTargetError::TooLong);
    }
    let at = address.find('@').ok_or(ForwardTargetError::NoAt)?;
    if address.rfind('@') != Some(at) {
        return Err(ForwardTargetError::MultipleAt);
    }
    let (local, domain) = (&address[..at], &address[at + 1..]);

    if local.is_empty() || local.len() > MAX_LOCAL_PART_LEN || !is_valid_local_part(local) {
        return Err(ForwardTargetError::LocalPartInvalid);
    }
    if domain.is_empty() || domain.len() > MAX_DOMAIN_LEN || !is_valid_domain(domain) {
        return Err(ForwardTargetError::DomainInvalid);
    }

    if local_domains.iter().any(|d| d.eq_ignore_ascii_case(domain)) {
        return Err(ForwardTargetError::IsLocalDomain);
    }
    Ok(())
}
/// Pragmatic RFC 5321 dot-atom local-part: ASCII printables excluding spaces,
/// the structural `@`, and the angle-bracket path delimiters. We do not accept
/// quoted local-parts (`"a b"@…`) — they are typo-shaped for a config field and
/// `smtp-server.md:18` says reject when in doubt.
fn is_valid_local_part(local: &str) -> bool {
    local
        .bytes()
        .all(|b| b.is_ascii_graphic() && b != b'@' && b != b'<' && b != b'>')
}

/// A dotted host name: ≥2 dot-separated labels, each `[A-Za-z0-9-]+` with no
/// leading/trailing hyphen. (A dotless domain is almost always a typo for a
/// forward target, so we require the dot.)
fn is_valid_domain(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    labels.iter().all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}
// ── EmailFilter (wire row) ─────────────────────────────────────────
//
// On-wire shape of a stored filter row. Mirrors the JSON object the
// HTTP twin emits from `filter_to_json` minus the storage-internal
// `owner` blob (implicit in the actor scope and not on the wire — same
// shape as `FeedSubscription` in `bridges_ui.rs`).

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailFilter {
    pub id: i64,
    pub name: String,
    pub rules: Vec<EmailFilterRule>,
    /// `"all"` or `"any"` — handler enforces the same validation as the
    /// HTTP twin. CBOR `tstr` accepts arbitrary values.
    pub combination: String,
    pub action: EmailFilterAction,
    pub priority: i32,
    /// Sieve `continue` (`smtp-server.md` § Email filter rules): when `false`
    /// (default) a match is terminal (first-match-wins); when `true` the action
    /// is recorded and evaluation falls through to later rules (multi-action).
    /// `#[serde(default)]` keeps pre-`continue` rows + fixtures decoding.
    #[serde(default)]
    pub continue_on_match: bool,
    /// Creation epoch in milliseconds (matches the DB's `created_at`
    /// column and the HTTP twin's JSON shape).
    pub created_at: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.email.filters.list ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListEmailFiltersRequest {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListEmailFiltersReply {
    pub filters: Vec<EmailFilter>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.email.filters.create ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateEmailFilterRequest {
    pub name: String,
    pub rules: Vec<EmailFilterRule>,
    /// `"all"` or `"any"`.
    pub combination: String,
    pub action: EmailFilterAction,
    pub priority: i32,
    /// Sieve `continue` — see [`EmailFilter::continue_on_match`].
    #[serde(default)]
    pub continue_on_match: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateEmailFilterReply {
    pub id: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.email.filters.get ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GetEmailFilterRequest {
    pub id: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GetEmailFilterReply {
    pub filter: EmailFilter,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.email.filters.update ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateEmailFilterRequest {
    pub id: i64,
    pub name: String,
    pub rules: Vec<EmailFilterRule>,
    /// `"all"` or `"any"`.
    pub combination: String,
    pub action: EmailFilterAction,
    pub priority: i32,
    /// Sieve `continue` — see [`EmailFilter::continue_on_match`].
    #[serde(default)]
    pub continue_on_match: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateEmailFilterReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.email.filters.delete ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeleteEmailFilterRequest {
    pub id: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeleteEmailFilterReply {
    pub ok: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.email.send ───────────────────────────────────────────────
//
// Outbound submission from a user client. The actor's authenticated
// identity is the authoritative sender — the handler verifies the
// `From:` header's local part matches the actor's handle when the
// domain matches the deployment's configured email domain. Local
// recipients (in-domain) are delivered to their inboxes; remote
// recipients (out-of-domain) are enqueued onto the outbound queue
// the MTA polls via `fauna.bridges.fetch_outbound_due`.
//
// Wire shape: the RFC 5322 message goes on the wire as CBOR `bstr`
// (raw bytes — WS-RPC carries binary natively, so the base64 hop the
// HTTP twin required is gone). The reply collapses the HTTP twin's
// three legacy fast-path shapes (`{"delivered":"local"}`,
// `{"queued":true,"id":N}`, `{"local_delivered":N,"remote_queued":N}`)
// into one uniform counter triple — parity with the multi-recipient
// shape the route already returned in the general case.
//
// `forbid_replay=true` at 30 s — replaying outbound delivery on a
// dropped-then-recovered connection could double-send to remote MX.
// Same shape as `fauna.bridges.link` (the OAuth-flow precedent for
// "rare dangerous ops" in the spec).

/// Error code (and per-message import outcome reason) meaning **the mail
/// message is larger than the deployment can carry** — the raw RFC 5322 body
/// exceeds `fauna_mail::transport_limits::effective_max_raw_message_bytes`
/// (the admin `max_message_bytes` knob clamped to what can rest). The typed
/// first-party sibling of the SMTP perimeter's `552 5.3.4`
/// (`smtp-server.md` § Message size limits), so a client renders "message too
/// large" instead of surfacing a raw transport failure.
///
/// Shared by BOTH first-party legs so a client matches one identifier:
/// `fauna.email.send` returns it as an [`crate::RpcError`] code, and
/// `import_message` reports it as an [`ImportMessageOutcome::Errored`] `reason`
/// (the reason-as-discriminator convention `Skipped` already uses). Precedent:
/// `fauna.mls.too_large` (`mls_replica.rs`). The **inline-ceiling** refusal that
/// keeps an over-frame send from severing the WS connection is a distinct,
/// client-side pre-check (a clients-area follow-on); this code is the nest's
/// authoritative product-ceiling enforcement.
pub const MESSAGE_TOO_LARGE_CODE: &str = "fauna.email.too_large";

/// The nest's From-handle refusal on `fauna.email.send`
/// (`email_handlers.rs::permission_denied`, the gate mail-app-surface.md
/// § First-party client send calls "sender-handle verification"). Covers both of
/// its arms: the caller has no handle at all, and the caller's From local part
/// is not their handle.
///
/// Named here so [`crate::RpcError::localized`] can map it to a real string.
/// The nest passes `"error.email.permission_denied"` as the error's *message*
/// and puts the human reason in `details` — which no client renders — so
/// before this mapping existed the bare identifier reached users' screens.
pub const SENDER_HANDLE_DENIED_CODE: &str = "fauna.email.permission_denied";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendEmailRequest {
    /// Recipients — full `local@domain` addresses. In-domain recipients
    /// (matching the deployment's configured email domain) are
    /// delivered locally; out-of-domain recipients are enqueued for
    /// outbound delivery.
    pub recipients: Vec<String>,
    /// Raw RFC 5322 message bytes. CBOR `bstr` on the wire (no base64
    /// envelope — WS-RPC is binary-clean).
    #[serde(with = "serde_bytes")]
    pub raw_rfc5322: Vec<u8>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SendEmailReply {
    /// Count of recipients delivered to in-domain inboxes (0 when all
    /// recipients are remote).
    pub local_delivered: u32,
    /// Count of recipients enqueued onto the outbound queue (0 when
    /// all recipients are local).
    pub remote_queued: u32,
    /// Per-recipient error strings if the outbound enqueue partially
    /// failed (the local delivery, if any, may still have succeeded).
    /// Empty on success.
    pub remote_errors: Vec<String>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.email.inbox.fetch ────────────────────────────────────────
//
// The inbound twin of `fauna.email.send` — a User-class, **caller-scoped**
// read of the calling actor's own `INBOX`. Shaped after
// `fauna.conversations.channel.fetch`: a UID-cursor pager over the
// nest-backed sealed mail store (the new Go-mail-bridge path —
// `bridge_imap_messages` placement + the `__mail/<actor>` segment store,
// NOT the deprecated `inbox`/`email_aliases` stack).
//
// There is deliberately **no `actor_id` request field**: the reading
// actor is the authenticated caller, so a caller can only ever read its
// own mailbox (caller-scoping by construction — adding an `actor_id`
// would re-introduce the BridgeMda `fauna.bridges.list_messages` shape).
//
// `sealed_envelope` is exactly the canonical `MailRecordEnvelope` bytes
// `segments::mail::read_envelopes_bulk` returns — opaque to the nest. The
// client decrypts it (the nest holds no opening key in encrypted mode —
// `conversations.md` § Receiving …). `forbid_replay=false` (idempotent
// read); see `kind.rs::register_email_kinds` for the 60 s deadline
// rationale (bodies can be large, like `fetch_message_ciphertext`).
//
// Float-free: every field is an int or `bstr`.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InboxFetchRequest {
    /// Cursor — return INBOX messages with `uid > after_uid`. `0` (the
    /// `Default`) starts from the beginning. Page until `more` is false.
    pub after_uid: u32,
    /// Max messages per page. `0` selects the handler default; the
    /// handler clamps to a small cap (bodies are large — far smaller
    /// than `channel.fetch`'s page size).
    pub limit: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InboxFetchReply {
    pub messages: Vec<InboxMessage>,
    /// `true` when more pages remain past this one (detected via a
    /// `limit+1` sentinel fetch). Re-call with `after_uid` = the last
    /// message's `uid`.
    pub more: bool,
    /// The fetched mailbox's `HIGHESTMODSEQ`, read **before** the page's rows
    /// were — so any flag write racing the page carries a higher modseq and is
    /// delivered again by `fauna.email.inbox.flag_changes` rather than missed.
    /// The client keeps the value from the **first** page of its launch drain
    /// as its `flag_changes` baseline (`mail-app-surface.md` § Read state).
    /// Additive (`#[serde(default)]`): `0` (an empty mailbox) is read by a client as "no flag-change feed here".
    #[serde(default)]
    pub highest_modseq: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InboxMessage {
    /// INBOX UID — monotonic per mailbox; the paging cursor.
    pub uid: u32,
    /// 32-byte server-assigned message id (the segment-record id).
    #[serde(with = "serde_bytes")]
    pub message_id: Vec<u8>,
    /// Epoch **seconds** (the floor `timestamp`, matching
    /// `fetch_message_ciphertext`). The client converts to ms.
    pub internal_date: i64,
    /// Opaque canonical `MailRecordEnvelope` bytes — exactly what
    /// `read_envelopes_bulk` returns. The client opens it with
    /// `open_mail_record` after unwrapping the outer segment envelope.
    #[serde(with = "serde_bytes")]
    pub sealed_envelope: Vec<u8>,
    /// The message's IMAP flag/keyword set (e.g. `\Seen`, `\Junk`,
    /// `$FaunaSpamScored`), split from the `bridge_imap_messages.flags`
    /// column. Additive (`#[serde(default)]` → an unflagged message decodes to
    /// empty). The **on-device spam scorer** reads it to skip any
    /// message already carrying the `$FaunaSpamScored` watermark — the
    /// cross-agent coordination channel that keeps the Fauna app and a
    /// third-party IMAP MUA from re-scoring each other's messages
    /// (`mail-spam.md` § Re-file timing, § Wire shapes).
    #[serde(default)]
    pub flags: Vec<String>,
    /// When the stored **outer** `MailRecordEnvelope` exceeds the WS-RPC frame
    /// budget, [`Self::sealed_envelope`] is empty and the envelope bytes ride
    /// the bulk-byte plane by reference (the ordered blake3 chunk-hash list +
    /// total). The client GETs each chunk over the **open** download route
    /// (`GET /api/v1/chunks/{hash}` — no token; confidentiality is
    /// cryptographic, not transport-scoped), joins them
    /// (`fauna_mail::body_ref::join_sealed_mail_body_checked` pins the total and
    /// fails closed on a wrong/short/reordered list), and opens the result
    /// exactly as an inline `sealed_envelope`. The client-feed twin of the MDA
    /// path's `FetchMessageCiphertextReply::Found.body_ref`
    /// (`smtp-server.md` § Message size limits — the client-feed reference leg).
    ///
    /// Additive (`#[serde(default, skip_serializing_if = "Option::is_none")]`):
    /// a message at or under the frame always rides inline and encodes
    /// with no `body_ref` key, so a reference only ever appears for a message
    /// whose stored envelope is too large for the feed to carry inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_ref: Option<crate::bridge_routing::MailBodyRef>,
    /// The record's **seal instant** — `segment_records.stored_at` (epoch
    /// **seconds**, the /1000 of the SQL mirror of `MailFloorMetadata::stored_at`).
    /// The content-sealing-epochs classification basis for the client's epoch
    /// opener chain (encryption-at-rest.md § Capability tiering; design § 4):
    /// a record sealed under `K_{epoch_of(stored_at)}` must be trialed off
    /// `stored_at`, NOT [`Self::internal_date`] — for **imported** mail the two
    /// diverge by design (the import carries a historical `internal_date` while
    /// its seal keyed off import-time now), and classifying off
    /// `internal_date` would try only epochs older than the seal's and miss the
    /// body. The MDA-path twin is `FetchMessageCiphertextReply::Found.stored_at`.
    ///
    /// Always on the wire. `0` = unknown: the storing nest's append-time clock
    /// read failed, and such a record is standing-sealed, so the reader's epoch
    /// chain ends on its standing arm. Never substitute `internal_date` for it.
    pub stored_at: i64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.email.apply_spam_disposition ─────────────────────────────
//
// The on-device spam scorer's outcome, applied by a `User`-class Fauna
// app to its OWN INBOX (`mail-spam.md` § Wire shapes, § Re-file timing).
// **Least-privilege:** it can express ONLY "watermark these UIDs as scored
// + move this subset INBOX→Junk" — never an arbitrary flag or mailbox. The
// nest executes it blindly (it learns the placement change, never the score
// or which n-grams matched — `content-scoring.md` § Stages). This is the
// `User`-class re-file the Fauna app needs; the `BridgeMda`-only
// `fauna.bridges.{store_flags,move}` the MDA uses are unreachable to a
// client, and the underlying `db` machinery is identical.

/// The internal IMAP keyword the on-device scorer (Fauna app) and the
/// MDA both stamp on a scored message so neither re-scores it. MUST match
/// the Go MDA's `spamScoredKeyword` (`bins/fauna-bridges/internal/mda/
/// imap/spam_score.go`). Internal/invisible-migratable per `mail-spam.md`
/// § Re-file timing.
pub const SPAM_SCORED_KEYWORD: &str = "$FaunaSpamScored";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ApplySpamDispositionRequest {
    /// The caller's own INBOX UIDs the on-device scorer just scored. Every
    /// one is stamped with the internal `$FaunaSpamScored` keyword (the same
    /// watermark the MDA sets) so neither agent re-scores it.
    pub scored_uids: Vec<u32>,
    /// The subset of `scored_uids` the scorer classified as spam — moved
    /// INBOX→Junk (watermark-before-move, so the moved rows carry the
    /// keyword and a later "not spam" move-back is not re-Junked). MUST be a
    /// subset of `scored_uids`; the handler rejects a stray UID.
    pub junk_uids: Vec<u32>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ApplySpamDispositionReply {
    /// How many of the caller's `scored_uids` were present in INBOX and
    /// watermarked (missing UIDs are silently skipped by the STORE).
    pub watermarked: u32,
    /// How many messages were actually moved INBOX→Junk (missing UIDs are
    /// silently skipped by `apply_move`).
    pub moved_to_junk: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.email.inbox.mark_seen / fauna.email.inbox.flag_changes ──
//
// A Fauna app's mail read state IS the message's IMAP `\Seen` flag
// (`conversation-read-state.md` § Mail: `\Seen` is the marker); these two
// kinds are the wire an app writes and syncs it over (`mail-app-surface.md`
// § Read state). Both are `User`-class and caller-scoped like the feeds — no
// `actor_id` field, a caller only ever touches its own `INBOX`.

/// `fauna.email.inbox.mark_seen` — add `\Seen` to each named `INBOX` UID that
/// lacks it, and nothing else: no other flag, no removal, no other mailbox. An
/// unknown or expunged UID is skipped without error and a repeat is a no-op.
/// Like `fauna.email.apply_spam_disposition` it is a named effect, never a
/// general flag door — `fauna.bridges.store_flags` stays `BridgeMda`-only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MarkSeenRequest {
    /// The caller's own `INBOX` UIDs to mark read.
    pub uids: Vec<u32>,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MarkSeenReply {
    /// How many rows gained `\Seen` (already-seen and unknown UIDs are not
    /// counted), so a replay answers `0`.
    pub updated: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.email.inbox.flag_changes` — the `INBOX` rows whose modseq is above
/// the cursor, oldest change first, each with its **whole current flag set**
/// (state, not an edit script, so redelivery is harmless). Expunged rows are
/// not reported; a message that left `INBOX` simply stops appearing.
///
/// The cursor is `(since_modseq, after_uid)`: one flag write stamps every row
/// it touches with the same modseq, so a page boundary can fall inside one
/// write, and `after_uid` resumes inside it. `after_uid == 0` (the default,
/// and the only value a client sends when not resuming a `more` page) means
/// "every row with `modseq > since_modseq`".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FlagChangesRequest {
    /// Report rows with `modseq > since_modseq` (and, with a non-zero
    /// `after_uid`, the rows at exactly `since_modseq` whose `uid > after_uid`).
    pub since_modseq: u64,
    /// Max rows per page. `0` selects the handler default; clamped to a cap.
    pub limit: u32,
    /// Tie-breaker inside one modseq when resuming a `more` page: the last
    /// change's `uid`, with `since_modseq` = that change's `modseq`. Additive
    /// (`#[serde(default)]`), `0` = not resuming.
    #[serde(default)]
    pub after_uid: u32,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One changed `INBOX` row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FlagChange {
    pub uid: u32,
    /// The row's whole current flag/keyword set.
    pub flags: Vec<String>,
    pub modseq: u64,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FlagChangesReply {
    /// Ordered by `(modseq, uid)` ascending.
    pub changes: Vec<FlagChange>,
    /// The `INBOX`'s `HIGHESTMODSEQ`, read before the rows were. When `more`
    /// is false the client's next cursor is `(highest_modseq, 0)`; when `more`
    /// is true it is the last change's `(modseq, uid)`.
    pub highest_modseq: u64,
    /// `true` when rows remain past this page.
    pub more: bool,
    /// Forward-compat catch-all: unknown keys from a newer peer are
    /// preserved here and re-emitted on encode (transport.md § Schema
    /// and forward-compat discipline, rule 4).
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn sample_filter() -> EmailFilter {
        EmailFilter {
            id: 42,
            name: "Move newsletters to Reading".into(),
            rules: vec![
                EmailFilterRule::SenderDomain {
                    domain: "newsletter.example.com".into(),
                },
                EmailFilterRule::SubjectContains {
                    text: "Weekly digest".into(),
                },
            ],
            combination: "any".into(),
            action: EmailFilterAction::FileInto {
                mailbox: "Reading".into(),
            },
            priority: 10,
            continue_on_match: true,
            created_at: 1_700_000_000_000,
            extra: Default::default(),
        }
    }

    fn sample_list_reply() -> ListEmailFiltersReply {
        ListEmailFiltersReply {
            filters: vec![
                sample_filter(),
                EmailFilter {
                    id: 99,
                    name: "Reject ex".into(),
                    rules: vec![EmailFilterRule::SenderIs {
                        address: "ex@example.com".into(),
                    }],
                    combination: "all".into(),
                    action: EmailFilterAction::Reject {
                        reason: "blocked".into(),
                    },
                    priority: 0,
                    continue_on_match: false,
                    created_at: 1_700_000_500_000,
                    extra: Default::default(),
                },
            ],
            extra: Default::default(),
        }
    }

    fn sample_create_request() -> CreateEmailFilterRequest {
        CreateEmailFilterRequest {
            name: "Auto-reply when on vacation".into(),
            rules: vec![EmailFilterRule::HeaderContains {
                name: "X-Priority".into(),
                value: "1".into(),
            }],
            combination: "all".into(),
            action: EmailFilterAction::AutoReply {
                subject: "Out of office".into(),
                body: "Back on Monday.".into(),
                interval_hours: 24,
            },
            priority: 5,
            continue_on_match: false,
            extra: Default::default(),
        }
    }

    fn sample_update_request() -> UpdateEmailFilterRequest {
        UpdateEmailFilterRequest {
            id: 17,
            name: "Updated name".into(),
            rules: vec![
                EmailFilterRule::BodyContains {
                    text: "spam".into(),
                },
                EmailFilterRule::HeaderExists {
                    name: "List-Unsubscribe".into(),
                },
            ],
            combination: "any".into(),
            action: EmailFilterAction::AddLabel {
                label: "Newsletters".into(),
            },
            priority: 20,
            continue_on_match: true,
            extra: Default::default(),
        }
    }

    #[test]
    fn list_request_round_trips() {
        let req = ListEmailFiltersRequest {};
        let bytes = encode_canonical(&req).unwrap();
        let decoded: ListEmailFiltersRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn list_reply_round_trips() {
        let reply = sample_list_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: ListEmailFiltersReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn list_reply_canonical_re_encodes_identically() {
        let reply = sample_list_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: ListEmailFiltersReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn create_request_round_trips() {
        let req = sample_create_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: CreateEmailFilterRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn create_request_canonical_re_encodes_identically() {
        let req = sample_create_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: CreateEmailFilterRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn create_reply_round_trips() {
        let reply = CreateEmailFilterReply {
            id: 17,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: CreateEmailFilterReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn get_request_round_trips() {
        let req = GetEmailFilterRequest {
            id: 42,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: GetEmailFilterRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn get_reply_round_trips() {
        let reply = GetEmailFilterReply {
            filter: sample_filter(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: GetEmailFilterReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn update_request_round_trips() {
        let req = sample_update_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: UpdateEmailFilterRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn update_request_canonical_re_encodes_identically() {
        let req = sample_update_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: UpdateEmailFilterRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn update_reply_round_trips() {
        let reply = UpdateEmailFilterReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: UpdateEmailFilterReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn delete_request_round_trips() {
        let req = DeleteEmailFilterRequest {
            id: 17,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: DeleteEmailFilterRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn delete_reply_round_trips() {
        let reply = DeleteEmailFilterReply {
            ok: true,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: DeleteEmailFilterReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn rule_round_trips_all_variants() {
        let variants = vec![
            EmailFilterRule::SenderIs {
                address: "a@b.c".into(),
            },
            EmailFilterRule::SenderDomain {
                domain: "b.c".into(),
            },
            EmailFilterRule::SubjectContains { text: "hi".into() },
            EmailFilterRule::BodyContains { text: "yo".into() },
            EmailFilterRule::HeaderExists {
                name: "X-Foo".into(),
            },
            EmailFilterRule::HeaderContains {
                name: "X-Foo".into(),
                value: "bar".into(),
            },
            // Scaled int — must survive strict dag-cbor (no float rejection).
            EmailFilterRule::SpamScoreAtLeast { milli: 7_500 },
        ];
        for r in variants {
            let bytes = encode_canonical(&r).unwrap();
            let decoded: EmailFilterRule = decode(&bytes).unwrap();
            assert_eq!(r, decoded);
        }
    }

    /// A newer build's filter vocabulary: one condition and two actions (a unit
    /// and a struct variant) this build does not know. Stands in for that
    /// writer in the unknown-arm tests below (`transport.md` § Schema and
    /// forward-compat discipline, rule 3).
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    enum NewerPeerFilterRule {
        SenderIs { address: String },
        ListIdIs { list_id: String },
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    enum NewerPeerFilterAction {
        Allow,
        Quarantine,
        Snooze { hours: u32 },
    }

    /// The `EmailFilter` a newer build writes, its rule and action drawn from
    /// the newer vocabulary.
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct NewerPeerEmailFilter {
        id: i64,
        name: String,
        rules: Vec<NewerPeerFilterRule>,
        combination: String,
        action: NewerPeerFilterAction,
        priority: i32,
        continue_on_match: bool,
        created_at: i64,
    }

    /// A filter carrying a rule and an action this build cannot read still
    /// decodes, both land in their `Unknown` arm, and re-encoding it gives the
    /// exact bytes the newer writer produced — so an older app or nest that
    /// echoes the filter back (an edit, a list → update) never rewrites it.
    #[test]
    fn unknown_rule_and_action_from_a_newer_writer_are_carried_byte_for_byte() {
        for action in [
            NewerPeerFilterAction::Quarantine,
            NewerPeerFilterAction::Snooze { hours: 4 },
        ] {
            let newer = NewerPeerEmailFilter {
                id: 7,
                name: "lists".into(),
                rules: vec![
                    NewerPeerFilterRule::SenderIs {
                        address: "a@b.c".into(),
                    },
                    NewerPeerFilterRule::ListIdIs {
                        list_id: "dev.example.org".into(),
                    },
                ],
                combination: "any".into(),
                action,
                priority: 1,
                continue_on_match: false,
                created_at: 1,
            };
            let bytes = encode_canonical(&newer).unwrap();
            let decoded: EmailFilter = decode(&bytes).unwrap();
            assert_eq!(
                decoded.rules[0],
                EmailFilterRule::SenderIs {
                    address: "a@b.c".into()
                }
            );
            assert!(matches!(decoded.rules[1], EmailFilterRule::Unknown(_)));
            assert!(matches!(decoded.action, EmailFilterAction::Unknown(_)));
            assert_eq!(encode_canonical(&decoded).unwrap(), bytes);
        }
    }

    /// The restrictive reading of an unknown arm: it is shown neutral, never as
    /// a known action, and no edit form opens on it (a form cannot represent
    /// it, so saving would replace it).
    #[test]
    fn unknown_rule_and_action_are_shown_neutral_and_never_open_in_a_form() {
        let rule: EmailFilterRule = decode(
            &encode_canonical(&NewerPeerFilterRule::ListIdIs {
                list_id: "x".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let action: EmailFilterAction =
            decode(&encode_canonical(&NewerPeerFilterAction::Quarantine).unwrap()).unwrap();
        assert_eq!(describe_filter_rule(&rule), None);
        assert_eq!(describe_filter_action(&action), None);
        assert_eq!(
            filter_action_label(&action),
            LocalizedText::key("common.unknown")
        );
        assert!(!filter_is_editable_for(
            std::slice::from_ref(&rule),
            &EmailFilterAction::Allow,
            SUPPORTED_ACTION_KINDS
        ));
        let known = EmailFilterRule::SenderIs {
            address: "a@b.c".into(),
        };
        assert!(!filter_is_editable_for(
            &[known],
            &action,
            SUPPORTED_ACTION_KINDS
        ));
    }

    /// The `Forward` variant without the `redirect` field: a test-only twin that
    /// names only `address`. Stands in for a reader or writer that does not
    /// name `redirect` in the two serde tests below.
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    enum RedirectlessFilterAction {
        Forward { address: String },
    }

    /// The serde-default pin (`version-compatibility.md` I4): a `Forward` action
    /// serialized without a `redirect` key decodes with the field defaulted to
    /// `false` — the goal's `copy` default (`mail-forwarding.md` § Per-rule
    /// "forward to").
    #[test]
    fn forward_action_without_redirect_defaults_to_copy() {
        let bare = RedirectlessFilterAction::Forward {
            address: "bob@example.com".into(),
        };
        let bytes = encode_canonical(&bare).unwrap();
        let decoded: EmailFilterAction = decode(&bytes).unwrap();
        assert_eq!(
            decoded,
            EmailFilterAction::Forward {
                address: "bob@example.com".into(),
                redirect: false,
            }
        );
    }

    /// The unknown-key-tolerance pin: a `redirect` rule is *tolerated* by a
    /// reader that cannot name the field (serde ignores unknown keys — no
    /// `deny_unknown_fields` on the variant), decoding as the `copy`-shaped
    /// `Forward` it knows. The field is dropped, not preserved: the variant has
    /// no `extra` catch-all, so outcome 7 ("never opens in a form that would
    /// lose part of it") does NOT rest on relay fidelity — it rests on the
    /// shared form round-tripping the copy mode, and on a form without the Forward inputs
    /// never opening a `Forward` (the next tests).
    #[test]
    fn forward_redirect_is_tolerated_by_a_reader_without_the_field() {
        let newer = EmailFilterAction::Forward {
            address: "bob@example.com".into(),
            redirect: true,
        };
        let bytes = encode_canonical(&newer).unwrap();
        let bare: RedirectlessFilterAction = decode(&bytes).unwrap();
        assert_eq!(
            bare,
            RedirectlessFilterAction::Forward {
                address: "bob@example.com".into(),
            }
        );
        // And the same bytes round-trip losslessly on a current reader.
        let same: EmailFilterAction = decode(&bytes).unwrap();
        assert_eq!(same, newer);
    }

    /// Outcome 7 for a `Forward`: the shared form opens it with its
    /// destination and copy mode — `redirect` included — and saving the
    /// untouched form writes back the identical action.
    #[test]
    fn forward_action_round_trips_through_the_shared_form_in_both_copy_modes() {
        for redirect in [false, true] {
            let action = EmailFilterAction::Forward {
                address: "bob@example.com".into(),
                redirect,
            };
            let inputs = describe_filter_action(&action).expect("Forward is form-covered");
            assert_eq!(inputs.kind, "Forward");
            assert_eq!(inputs.forward_address, "bob@example.com");
            assert_eq!(inputs.keep_local_copy, !redirect);
            assert_eq!(encode_filter_action(&inputs).unwrap(), action);
            let mut filter = sample_filter();
            filter.rules.truncate(1);
            filter.action = action;
            assert!(
                filter_is_editable(&filter),
                "a Forward rule (redirect={redirect}) opens in the Forward-aware form"
            );
        }
    }

    /// Outcome 7 for a form that lacks the Forward inputs: gating on a kind
    /// set without `Forward` it never opens a `Forward` in either copy mode,
    /// so it cannot save one back without its destination.
    #[test]
    fn forward_action_is_never_editable_in_a_form_without_forward_inputs() {
        const WITHOUT_FORWARD: &[&str] = &["Allow", "Discard", "Reject"];
        for redirect in [false, true] {
            let mut filter = sample_filter();
            filter.rules.truncate(1);
            filter.action = EmailFilterAction::Forward {
                address: "bob@example.com".into(),
                redirect,
            };
            assert!(
                !filter_is_editable_for(&filter.rules, &filter.action, WITHOUT_FORWARD),
                "a Forward rule (redirect={redirect}) must not open in a form without the Forward inputs"
            );
            filter.action = EmailFilterAction::Discard;
            assert!(filter_is_editable_for(
                &filter.rules,
                &filter.action,
                WITHOUT_FORWARD
            ));
        }
    }

    #[test]
    fn send_request_round_trips() {
        let req = SendEmailRequest {
            recipients: vec!["bob@example.com".into(), "carol@other.example".into()],
            raw_rfc5322:
                b"From: alice@example.com\r\nTo: bob@example.com\r\nSubject: hi\r\n\r\nHello."
                    .to_vec(),
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: SendEmailRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn send_request_canonical_re_encodes_identically() {
        let req = SendEmailRequest {
            recipients: vec!["bob@example.com".into()],
            raw_rfc5322: vec![0x00, 0x01, 0x02, 0x7f, 0x80, 0xff],
            extra: Default::default(),
        };
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: SendEmailRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn send_reply_round_trips() {
        let reply = SendEmailReply {
            local_delivered: 1,
            remote_queued: 2,
            remote_errors: vec!["bob@example.com: relay failed".into()],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SendEmailReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn send_reply_default_is_empty() {
        let reply = SendEmailReply::default();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: SendEmailReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
        assert_eq!(decoded.local_delivered, 0);
        assert_eq!(decoded.remote_queued, 0);
        assert!(decoded.remote_errors.is_empty());
    }

    #[test]
    fn inbox_fetch_request_round_trips() {
        let req = InboxFetchRequest {
            after_uid: 41,
            limit: 25,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: InboxFetchRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn inbox_fetch_request_default_is_zero_cursor() {
        let req = InboxFetchRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: InboxFetchRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert_eq!(decoded.after_uid, 0);
        assert_eq!(decoded.limit, 0);
    }

    #[test]
    fn inbox_fetch_reply_round_trips() {
        let reply = InboxFetchReply {
            messages: vec![
                InboxMessage {
                    uid: 1,
                    message_id: vec![0xAA; 32],
                    internal_date: 1_715_000_000,
                    // A seal instant distinct from internal_date (imported mail) —
                    // must round-trip.
                    stored_at: 1_714_900_000,
                    // Non-UTF-8 bytes — must survive as a CBOR `bstr`.
                    sealed_envelope: vec![0x00, 0x01, 0x80, 0xff, 0x7f],
                    flags: vec!["\\Seen".to_string(), "$FaunaSpamScored".to_string()],
                    body_ref: None,
                    extra: Default::default(),
                },
                // An over-frame message: empty inline envelope, a `body_ref`
                // (the client-feed reference leg) — must round-trip.
                InboxMessage {
                    uid: 2,
                    message_id: vec![0xBB; 32],
                    internal_date: 1_715_000_500,
                    stored_at: 1_715_000_600,
                    sealed_envelope: Vec::new(),
                    flags: vec![],
                    body_ref: Some(crate::bridge_routing::MailBodyRef {
                        chunk_hashes: vec![
                            serde_bytes::ByteBuf::from(vec![0x11; 32]),
                            serde_bytes::ByteBuf::from(vec![0x22; 32]),
                        ],
                        total_bytes: 6_000_000,
                    }),
                    extra: Default::default(),
                },
            ],
            more: true,
            highest_modseq: 17,
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: InboxFetchReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn inbox_fetch_reply_canonical_re_encodes_identically() {
        let reply = InboxFetchReply {
            messages: vec![InboxMessage {
                uid: 9,
                message_id: vec![0xCC; 32],
                internal_date: 1_715_999_999,
                stored_at: 1_716_000_000,
                sealed_envelope: vec![0x42; 64],
                flags: vec!["$FaunaSpamScored".to_string()],
                body_ref: None,
                extra: Default::default(),
            }],
            more: false,
            highest_modseq: 3,
            extra: Default::default(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: InboxFetchReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    /// The additive `body_ref` field is **wire-invisible when `None`**
    /// (`#[serde(skip_serializing_if)]`), so an inline message encodes
    /// with no `body_ref` key at all
    /// (`smtp-server.md` § Message size limits, the client-feed reference leg).
    #[test]
    fn inbox_message_body_ref_is_wire_invisible_when_absent() {
        let inline = InboxMessage {
            uid: 7,
            message_id: vec![0xDD; 32],
            internal_date: 1_716_000_000,
            stored_at: 1_716_000_100,
            sealed_envelope: b"an inline envelope".to_vec(),
            flags: vec!["\\Seen".to_string()],
            body_ref: None,
            extra: Default::default(),
        };
        let encoded = encode_canonical(&inline).unwrap();
        // The inline shape: a map with exactly the four inline keys plus
        // `flags`. A `None` body_ref must add NOTHING (no `body_ref` key, not
        // even a null), or a reader's decode would see an unexpected shape.
        assert!(
            !encoded.windows(b"body_ref".len()).any(|w| w == b"body_ref"),
            "a None body_ref must not appear on the wire at all"
        );
        // And it still round-trips to the same value.
        let decoded: InboxMessage = decode(&encoded).unwrap();
        assert_eq!(decoded, inline);
        assert!(decoded.body_ref.is_none());
    }

    /// `stored_at` (the content-sealing-epochs seal instant) is always on the
    /// wire — the unknown `0` included, so a reader never mistakes an absent
    /// key for a value — and a message without it is refused rather than
    /// defaulted: no nest omits it.
    #[test]
    fn inbox_message_stored_at_is_always_on_the_wire() {
        let msg = InboxMessage {
            uid: 8,
            message_id: vec![0xEE; 32],
            internal_date: 1_716_100_000,
            stored_at: 0,
            sealed_envelope: b"envelope".to_vec(),
            flags: vec![],
            body_ref: None,
            extra: Default::default(),
        };
        let encoded = encode_canonical(&msg).unwrap();
        let mut map: BTreeMap<String, Value> = decode(&encoded).unwrap();
        assert!(
            map.contains_key("stored_at"),
            "the unknown 0 must still be on the wire"
        );
        assert_eq!(decode::<InboxMessage>(&encoded).unwrap(), msg);

        map.remove("stored_at");
        let without = encode_canonical(&map).unwrap();
        assert!(
            decode::<InboxMessage>(&without).is_err(),
            "a message without stored_at must be refused, not defaulted"
        );
    }

    #[test]
    fn inbox_fetch_reply_default_is_empty() {
        let reply = InboxFetchReply::default();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: InboxFetchReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
        assert!(decoded.messages.is_empty());
        assert!(!decoded.more);
    }

    #[test]
    fn action_round_trips_all_variants() {
        let variants = vec![
            EmailFilterAction::Allow,
            EmailFilterAction::Discard,
            EmailFilterAction::Reject {
                reason: "spam".into(),
            },
            EmailFilterAction::FileInto {
                mailbox: "Archive".into(),
            },
            EmailFilterAction::Forward {
                address: "bob@example.com".into(),
                redirect: true,
            },
            EmailFilterAction::AutoReply {
                subject: "Out".into(),
                body: "Back Monday".into(),
                interval_hours: 12,
            },
            EmailFilterAction::AddLabel {
                label: "important".into(),
            },
        ];
        for a in variants {
            let bytes = encode_canonical(&a).unwrap();
            let decoded: EmailFilterAction = decode(&bytes).unwrap();
            assert_eq!(a, decoded);
        }
    }

    // ── encode_filter_rule / encode_filter_action (UI dropdown → typed wire) ──

    #[test]
    fn encode_rule_covers_the_canonical_single_value_kinds() {
        assert_eq!(
            encode_filter_rule("SenderIs", "a@b.com").unwrap(),
            EmailFilterRule::SenderIs {
                address: "a@b.com".into()
            }
        );
        assert_eq!(
            encode_filter_rule("SenderDomain", "b.com").unwrap(),
            EmailFilterRule::SenderDomain {
                domain: "b.com".into()
            }
        );
        assert_eq!(
            encode_filter_rule("SubjectContains", "sale").unwrap(),
            EmailFilterRule::SubjectContains {
                text: "sale".into()
            }
        );
        assert_eq!(
            encode_filter_rule("BodyContains", "viagra").unwrap(),
            EmailFilterRule::BodyContains {
                text: "viagra".into()
            }
        );
        assert_eq!(
            encode_filter_rule("HeaderExists", "List-Id").unwrap(),
            EmailFilterRule::HeaderExists {
                name: "List-Id".into()
            }
        );
    }

    #[test]
    fn encode_rule_rejects_unknown_kind_instead_of_silent_fallback() {
        // The pre-lift clients all silently coerced an unknown kind to
        // `SenderIs` (or web fell through to an empty rule); the shared
        // encoder makes it an explicit error.
        let err = encode_filter_rule("sender", "x").unwrap_err();
        assert_eq!(err, UnknownFilterKind("sender".into()));
        assert!(encode_filter_rule("HeaderContains", "x").is_err());
        assert!(encode_filter_rule("", "x").is_err());
    }

    #[test]
    fn encode_action_covers_allow_discard_reject() {
        assert_eq!(
            encode_filter_action(&FilterActionInputs::new("Allow")).unwrap(),
            EmailFilterAction::Allow
        );
        assert_eq!(
            encode_filter_action(&FilterActionInputs::new("Discard")).unwrap(),
            EmailFilterAction::Discard
        );
        // Caller-supplied reason rides through verbatim.
        assert_eq!(
            encode_filter_action(&FilterActionInputs {
                reject_reason: "no bots".into(),
                ..FilterActionInputs::new("Reject")
            })
            .unwrap(),
            EmailFilterAction::Reject {
                reason: "no bots".into()
            }
        );
    }

    #[test]
    fn encode_action_reject_empty_reason_falls_back_to_canonical_default() {
        assert_eq!(
            encode_filter_action(&FilterActionInputs::new("Reject")).unwrap(),
            EmailFilterAction::Reject {
                reason: DEFAULT_REJECT_REASON.into()
            }
        );
    }

    #[test]
    fn encode_action_rejects_unknown_kind() {
        assert_eq!(
            encode_filter_action(&FilterActionInputs::new("discard")).unwrap_err(),
            FilterActionError::UnknownKind("discard".into())
        );
        assert!(encode_filter_action(&FilterActionInputs::new("FileInto")).is_err());
    }

    /// A version-locked form value: an unknown key is refused, never dropped.
    #[test]
    fn filter_action_inputs_refuse_an_unknown_key() {
        #[derive(Serialize)]
        struct Typo {
            kind: &'static str,
            forward_adress: &'static str,
        }
        let bytes = crate::codec::encode_canonical(&Typo {
            kind: "Forward",
            forward_adress: "bob@example.net",
        })
        .unwrap();
        assert!(crate::codec::decode_strict::<FilterActionInputs>(&bytes).is_err());
        let known = crate::codec::encode_canonical(&FilterActionInputs::new("Forward")).unwrap();
        assert_eq!(
            crate::codec::decode_strict::<FilterActionInputs>(&known).unwrap(),
            FilterActionInputs::new("Forward")
        );
    }

    /// The keep-a-local-copy checkbox defaults to checked (`copy`); unchecking
    /// it is `redirect` (`mail-forwarding.md` § Per-rule "forward to").
    #[test]
    fn encode_action_forward_maps_the_checkbox_onto_the_copy_mode() {
        let copy = FilterActionInputs {
            forward_address: "bob@example.net".into(),
            ..FilterActionInputs::new("Forward")
        };
        assert!(copy.keep_local_copy, "the checkbox starts checked");
        assert_eq!(
            encode_filter_action(&copy).unwrap(),
            EmailFilterAction::Forward {
                address: "bob@example.net".into(),
                redirect: false,
            }
        );
        let redirect = FilterActionInputs {
            keep_local_copy: false,
            ..copy
        };
        assert_eq!(
            encode_filter_action(&redirect).unwrap(),
            EmailFilterAction::Forward {
                address: "bob@example.net".into(),
                redirect: true,
            }
        );
    }

    /// The destination gets the nest's own rule-path check before the round
    /// trip, trimmed of surrounding whitespace — and, like the nest's rule
    /// path, no hosted-domain bar (only forward-all has one).
    #[test]
    fn encode_action_forward_validates_the_destination_like_the_nest_rule_path() {
        let forward = |address: &str| FilterActionInputs {
            forward_address: address.into(),
            ..FilterActionInputs::new("Forward")
        };
        assert_eq!(
            encode_filter_action(&forward("")).unwrap_err(),
            FilterActionError::ForwardAddress(ForwardTargetError::Empty)
        );
        assert_eq!(
            encode_filter_action(&forward("not-an-address")).unwrap_err(),
            FilterActionError::ForwardAddress(ForwardTargetError::NoAt)
        );
        assert_eq!(
            encode_filter_action(&forward("bob@nodot")).unwrap_err(),
            FilterActionError::ForwardAddress(ForwardTargetError::DomainInvalid)
        );
        assert_eq!(
            encode_filter_action(&forward("  bob@example.net ")).unwrap(),
            EmailFilterAction::Forward {
                address: "bob@example.net".into(),
                redirect: false,
            }
        );
    }

    #[test]
    fn validate_label_accepts_plain_keywords() {
        // Letters, digits, and non-special punctuation are all RFC 3501
        // ATOM-CHARs — the labels a sane user creates pass unchanged.
        // (non-ASCII is rejected; see validate_label_rejects_atom_specials_and_non_ascii)
        for ok in ["Newsletter", "Work-2026", "foo_bar", "a.b+c", "$Label"] {
            assert!(
                validate_label(ok).is_ok(),
                "{ok:?} should be a valid keyword"
            );
        }
    }

    #[test]
    fn validate_label_rejects_system_flags() {
        // A leading backslash is the IMAP system-flag namespace; `\Deleted`
        // would make matching mail EXPUNGE-eligible, `\Seen` silently read.
        assert_eq!(validate_label("\\Deleted"), Err(InvalidLabel::SystemFlag));
        assert_eq!(validate_label("\\Seen"), Err(InvalidLabel::SystemFlag));
        // A non-leading backslash is a generic illegal char.
        assert_eq!(
            validate_label("foo\\bar"),
            Err(InvalidLabel::IllegalChar('\\'))
        );
    }

    #[test]
    fn validate_label_rejects_whitespace_and_empty() {
        assert_eq!(validate_label(""), Err(InvalidLabel::Empty));
        // Internal whitespace would split into several keywords at ingest.
        assert_eq!(
            validate_label("two words"),
            Err(InvalidLabel::IllegalChar(' '))
        );
        assert_eq!(
            validate_label("tab\tlabel"),
            Err(InvalidLabel::IllegalChar('\t'))
        );
    }

    #[test]
    fn validate_label_rejects_atom_specials_and_non_ascii() {
        for (bad, ch) in [
            ("a(b", '('),
            ("a)b", ')'),
            ("a{b", '{'),
            ("a%b", '%'),
            ("a*b", '*'),
            ("a\"b", '"'),
            ("a]b", ']'),
        ] {
            assert_eq!(
                validate_label(bad),
                Err(InvalidLabel::IllegalChar(ch)),
                "{bad:?} must be rejected"
            );
        }
        // Non-ASCII is not an RFC 3501 CHAR (keywords are ASCII atoms).
        assert_eq!(validate_label("Café"), Err(InvalidLabel::IllegalChar('é')));
        // Control byte (e.g. a stray CR) is rejected.
        assert_eq!(validate_label("a\rb"), Err(InvalidLabel::IllegalChar('\r')));
    }

    #[test]
    fn validate_file_into_mailbox_refuses_what_inbound_mail_must_never_enter() {
        // `Sent` and `Drafts` hold only this account's own writing; the held
        // mailbox belongs to the hold path (email-filters.md § Email filter
        // rules).
        assert_eq!(
            validate_file_into_mailbox("Sent"),
            Err(InvalidFileIntoMailbox::Sent)
        );
        assert_eq!(
            validate_file_into_mailbox("Drafts"),
            Err(InvalidFileIntoMailbox::Drafts)
        );
        assert_eq!(
            validate_file_into_mailbox(GUARDIAN_HELD_MAILBOX),
            Err(InvalidFileIntoMailbox::GuardianHeld)
        );
    }

    #[test]
    fn validate_file_into_mailbox_accepts_every_other_target() {
        // The rest of the standard set, custom folders — a child of a refused
        // name included, since `Sent/2026` is a mailbox of its own — and a
        // differently-cased name, since the mailbox store matches names exactly.
        for ok in [
            "INBOX",
            "Archive",
            "Junk",
            "Trash",
            "Reports",
            "Sent/2026",
            "sent",
            "drafts",
        ] {
            assert!(
                validate_file_into_mailbox(ok).is_ok(),
                "{ok:?} should be a valid FileInto target"
            );
        }
    }

    #[test]
    fn describe_filter_rule_is_the_exact_inverse_of_encode_for_every_dialog_kind() {
        for (kind, value) in [
            ("SenderIs", "bob@example.com"),
            ("SenderDomain", "news.example"),
            ("SubjectContains", "invoice"),
            ("BodyContains", "unsubscribe"),
            ("HeaderExists", "List-Id"),
        ] {
            let rule = encode_filter_rule(kind, value).unwrap();
            assert_eq!(
                describe_filter_rule(&rule),
                Some((kind, value.to_string())),
                "{kind} did not round-trip"
            );
        }
    }

    #[test]
    fn describe_filter_rule_is_none_for_kinds_no_dialog_collects() {
        assert_eq!(
            describe_filter_rule(&EmailFilterRule::HeaderContains {
                name: "X-Spam".into(),
                value: "yes".into(),
            }),
            None
        );
        assert_eq!(
            describe_filter_rule(&EmailFilterRule::SpamScoreAtLeast { milli: 500 }),
            None
        );
    }

    #[test]
    fn describe_filter_action_is_the_exact_inverse_of_encode_for_every_dialog_kind() {
        for action in [
            EmailFilterAction::Allow,
            EmailFilterAction::Discard,
            EmailFilterAction::Reject {
                reason: "no bots".into(),
            },
            EmailFilterAction::Forward {
                address: "a@b.example".into(),
                redirect: true,
            },
        ] {
            let inputs = describe_filter_action(&action).expect("form-covered");
            assert!(SUPPORTED_ACTION_KINDS.contains(&inputs.kind.as_str()));
            assert_eq!(encode_filter_action(&inputs).unwrap(), action);
        }
    }

    #[test]
    fn describe_filter_action_is_none_for_kinds_no_dialog_collects() {
        for action in [
            EmailFilterAction::FileInto {
                mailbox: "Reading".into(),
            },
            EmailFilterAction::AutoReply {
                subject: "Away".into(),
                body: "Back soon".into(),
                interval_hours: 24,
            },
            EmailFilterAction::AddLabel {
                label: "Newsletters".into(),
            },
        ] {
            assert_eq!(describe_filter_action(&action), None, "{action:?}");
        }
    }

    #[test]
    fn filter_action_label_covers_every_variant_including_the_ones_no_dialog_collects() {
        assert_eq!(
            filter_action_label(&EmailFilterAction::Allow).key,
            "status.email_filters.action_allow"
        );
        assert_eq!(
            filter_action_label(&EmailFilterAction::Discard).key,
            "status.email_filters.action_discard"
        );
        assert_eq!(
            filter_action_label(&EmailFilterAction::Reject { reason: "x".into() }).key,
            "status.email_filters.action_reject"
        );
        assert_eq!(
            filter_action_label(&EmailFilterAction::FileInto {
                mailbox: "Reading".into(),
            })
            .key,
            "status.email_filters.action_file_into"
        );
        assert_eq!(
            filter_action_label(&EmailFilterAction::Forward {
                address: "a@b.example".into(),
                redirect: false,
            })
            .key,
            "status.email_filters.action_forward"
        );
        assert_eq!(
            filter_action_label(&EmailFilterAction::AutoReply {
                subject: "Away".into(),
                body: "Back soon".into(),
                interval_hours: 24,
            })
            .key,
            "status.email_filters.action_auto_reply"
        );
        assert_eq!(
            filter_action_label(&EmailFilterAction::AddLabel {
                label: "Newsletters".into(),
            })
            .key,
            "status.email_filters.action_add_label"
        );
    }

    #[test]
    fn filter_is_editable_true_for_a_single_dialog_covered_rule_and_action() {
        let filter = EmailFilter {
            id: 1,
            name: "Move newsletters".into(),
            rules: vec![EmailFilterRule::SenderDomain {
                domain: "news.example".into(),
            }],
            combination: "all".into(),
            action: EmailFilterAction::Allow,
            priority: 0,
            continue_on_match: false,
            created_at: 0,
            extra: Default::default(),
        };
        assert!(filter_is_editable(&filter));
    }

    #[test]
    fn filter_is_editable_false_for_multi_rule_or_dialog_uncovered_shapes() {
        let single_covered_rule = vec![EmailFilterRule::SenderIs {
            address: "a@b.example".into(),
        }];
        // Two rules — the dialog only ever produces one.
        let multi_rule = EmailFilter {
            id: 1,
            name: "n".into(),
            rules: vec![
                EmailFilterRule::SenderIs {
                    address: "a@b.example".into(),
                },
                EmailFilterRule::SenderDomain {
                    domain: "b.example".into(),
                },
            ],
            combination: "any".into(),
            action: EmailFilterAction::Allow,
            priority: 0,
            continue_on_match: false,
            created_at: 0,
            extra: Default::default(),
        };
        assert!(!filter_is_editable(&multi_rule));

        // A rule the dialog can't collect.
        let uncovered_rule = EmailFilter {
            id: 2,
            name: "n".into(),
            rules: vec![EmailFilterRule::SpamScoreAtLeast { milli: 500 }],
            combination: "all".into(),
            action: EmailFilterAction::Allow,
            priority: 0,
            continue_on_match: false,
            created_at: 0,
            extra: Default::default(),
        };
        assert!(!filter_is_editable(&uncovered_rule));

        // An action the dialog can't collect.
        let uncovered_action = EmailFilter {
            id: 3,
            name: "n".into(),
            rules: single_covered_rule,
            combination: "all".into(),
            action: EmailFilterAction::FileInto {
                mailbox: "Reading".into(),
            },
            priority: 0,
            continue_on_match: false,
            created_at: 0,
            extra: Default::default(),
        };
        assert!(!filter_is_editable(&uncovered_action));
    }

    // ── validate_forward_target ──

    const LOCAL_DOMAINS: &[&str] = &["fauna.example", "second.example"];

    #[test]
    fn forward_target_accepts_a_normal_external_address() {
        assert_eq!(
            validate_forward_target("bob@example.net", LOCAL_DOMAINS),
            Ok(())
        );
        assert_eq!(
            validate_forward_target("bob.smith+x@mail.co.uk", LOCAL_DOMAINS),
            Ok(())
        );
    }

    #[test]
    fn forward_target_rejects_local_domain_case_insensitively() {
        assert_eq!(
            validate_forward_target("bob@fauna.example", LOCAL_DOMAINS),
            Err(ForwardTargetError::IsLocalDomain),
        );
        assert_eq!(
            validate_forward_target("bob@Fauna.Example", LOCAL_DOMAINS),
            Err(ForwardTargetError::IsLocalDomain),
        );
    }

    #[test]
    fn forward_target_rejects_structural_problems() {
        assert_eq!(
            validate_forward_target("", LOCAL_DOMAINS),
            Err(ForwardTargetError::Empty)
        );
        assert_eq!(
            validate_forward_target("noat", LOCAL_DOMAINS),
            Err(ForwardTargetError::NoAt)
        );
        assert_eq!(
            validate_forward_target("a@b@c.com", LOCAL_DOMAINS),
            Err(ForwardTargetError::MultipleAt)
        );
    }

    #[test]
    fn forward_target_rejects_bad_local_part() {
        assert_eq!(
            validate_forward_target("@example.net", LOCAL_DOMAINS),
            Err(ForwardTargetError::LocalPartInvalid)
        );
        assert_eq!(
            validate_forward_target("bob smith@example.net", LOCAL_DOMAINS),
            Err(ForwardTargetError::LocalPartInvalid),
        );
        let long_local = format!("{}@example.net", "a".repeat(65));
        assert_eq!(
            validate_forward_target(&long_local, LOCAL_DOMAINS),
            Err(ForwardTargetError::LocalPartInvalid)
        );
    }

    #[test]
    fn forward_target_rejects_bad_domain() {
        assert_eq!(
            validate_forward_target("bob@", LOCAL_DOMAINS),
            Err(ForwardTargetError::DomainInvalid)
        );
        assert_eq!(
            validate_forward_target("bob@nodot", LOCAL_DOMAINS),
            Err(ForwardTargetError::DomainInvalid)
        );
        assert_eq!(
            validate_forward_target("bob@-bad.com", LOCAL_DOMAINS),
            Err(ForwardTargetError::DomainInvalid)
        );
        assert_eq!(
            validate_forward_target("bob@bad-.com", LOCAL_DOMAINS),
            Err(ForwardTargetError::DomainInvalid)
        );
        assert_eq!(
            validate_forward_target("bob@dou..ble.com", LOCAL_DOMAINS),
            Err(ForwardTargetError::DomainInvalid)
        );
    }

    #[test]
    fn forward_target_rejects_overlong_path() {
        let too_long = format!("a@{}.com", "b".repeat(260));
        assert_eq!(
            validate_forward_target(&too_long, LOCAL_DOMAINS),
            Err(ForwardTargetError::TooLong)
        );
    }
}
