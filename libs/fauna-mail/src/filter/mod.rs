//! Server-side email filter-rule evaluation — the pure ordered multi-action
//! engine behind `fauna.email.filters.*` (first-match-wins unless a rule is
//! marked `continue`; see [`evaluate`]).
//!
//! **Where this runs (LBC-1/-3).** Filter rules are
//! evaluated **at the Go MTA perimeter, pre-seal, in both storage modes** —
//! `docs/goal/behavior/mail-forwarding.md:217` ("Forwarding is an MTA-perimeter
//! decision, happening on plaintext bytes the MTA already holds in-process
//! during the inbound DATA stage") and `:278` ("building a 'rule evaluator runs
//! on the stored ciphertext' path in encrypted mode is the wrong shape and would
//! break the storage-mode invariant"). The rules themselves are *stored* in nest
//! (`email_filters`) and *fetched* by the MTA per-recipient at delivery time
//! (`fauna.bridges.fetch_recipient_filters`); this module is the **pure decision
//! half**, like `crate::scan` / `crate::spam` — the network/parse I/O is Go-side.
//! That keeps it std-only (no tokio/parser), so it sits in the `fauna-ffi`
//! default-features surface and the Go binding (`libs/fauna-mail-go`) calls it.
//!
//! **v1 condition set (LBC-2, `mail-forwarding.md:232`/`:260`).** Evaluable: the
//! envelope sender, the `Subject`, arbitrary headers, the **spam score**
//! (`smtp-server.md:784` — "a per-user filter rule acts on the score"), and the
//! decoded **body** ([`FilterCondition::BodyContains`]). Body matching is a
//! **case-insensitive substring** test over the message's decoded
//! `text/plain` + `text/html` parts as the MTA already holds them pre-seal
//! ([`FilterContext::body`]) — plaintext-floor in both storage modes, so it never
//! requires post-seal ciphertext access (LBC-3). The producer (the Go MTA)
//! size-caps the body it puts on the wire (`server.go`, 256 KiB) so a huge
//! message can't blow the eval/wire; HTML is matched as **raw decoded markup**
//! (no tag-stripping) in v1. Full RFC 5804 ManageSieve + the Sieve `body`
//! extension stay deferred (`imap-server.md` § Upstream-blocked gaps); this is
//! the simple-substring subset, not a Sieve engine.
//!
//! **No wire floats** (`serialization.md` strict dag-cbor decode rejects them):
//! the spam-score condition is a **milli-int** ([`FilterCondition::SpamScoreAtLeast`]),
//! the same per-mille scale as `crate::spam::combined_spam_score_milli` (the
//! `combinedMilli` the Go MTA computes at `server.go` before disposition).
//!
//! The [`FilterCondition`] / [`FilterAction`] variants mirror the wire enums in
//! `fauna_protocol::email` (`EmailFilterRule` / `EmailFilterAction`) plus the new
//! [`FilterCondition::SpamScoreAtLeast`]; the bridge↔nest boundary converts. This
//! crate does not depend on `fauna-protocol`, so the two type families are kept
//! aligned by review + the boundary conversion's tests.

/// How a filter's conditions combine (`smtp-server.md` § Email filter rules,
/// the wire `EmailFilter.combination` `"all"`/`"any"` string). Use
/// [`FilterCombination::from_wire`] at the boundary; an unrecognized string maps
/// to [`FilterCombination::All`], matching the legacy default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FilterCombination {
    /// Every condition must match (`all`). Vacuously true for an empty rule set.
    All,
    /// Any one condition matching is enough (`any`). False for an empty rule set.
    Any,
}

impl FilterCombination {
    /// Map the wire `combination` string. `"any"` (case-insensitive) →
    /// [`FilterCombination::Any`]; anything else (incl. `"all"` and unknown) →
    /// [`FilterCombination::All`] — the legacy `store_impl.rs` default.
    pub fn from_wire(s: &str) -> Self {
        if s.eq_ignore_ascii_case("any") {
            Self::Any
        } else {
            Self::All
        }
    }
}

/// A single match criterion. Mirrors `fauna_protocol::email::EmailFilterRule`
/// plus [`FilterCondition::SpamScoreAtLeast`] (this track's addition).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FilterCondition {
    /// Envelope sender (`MAIL FROM`) equals `address`, case-insensitive.
    SenderIs { address: String },
    /// The sender's domain (after the last `@`) equals `domain`, case-insensitive.
    SenderDomain { domain: String },
    /// The `Subject` contains `text` (case-insensitive substring).
    SubjectContains { text: String },
    /// The decoded body ([`FilterContext::body`] — `text/plain` + `text/html`)
    /// contains `text` (case-insensitive substring). HTML matches against raw
    /// decoded markup (no tag-stripping) in v1.
    BodyContains { text: String },
    /// A header named `name` is present (case-insensitive name match).
    HeaderExists { name: String },
    /// A header named `name` (case-insensitive) has a value containing `value`
    /// (case-insensitive substring).
    HeaderContains { name: String, value: String },
    /// The combined spam score (milli-int, per `crate::spam`) is `>= milli`.
    /// The "user-defined algorithm acts on the spam score" condition
    /// (`smtp-server.md:784`).
    SpamScoreAtLeast { milli: i32 },
}

/// What to do when a filter matches. Mirrors
/// `fauna_protocol::email::EmailFilterAction`.
///
/// The placement actions ([`FilterAction::FileInto`], [`FilterAction::AddLabel`],
/// [`FilterAction::Discard`], [`FilterAction::Allow`]) and [`FilterAction::Reject`]
/// are wired at the Go MTA perimeter; [`FilterAction::Forward`] is owned by
/// `mail-forwarding.md`; [`FilterAction::AutoReply`] is wired via the perimeter
/// loop-guard ([`auto_reply_decision`]) + compose
/// ([`crate::outbound::autoreply::compose_auto_reply`]) → sign →
/// `fauna.bridges.send_auto_reply` (atomic rate-limit + null-sender enqueue). The
/// evaluator returns the matched action regardless — composition + side effects
/// are the caller's job (`smtp-server.md` § Email filter rules spells out the
/// precedence).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FilterAction {
    /// Whitelist: deliver to INBOX, overriding any spam-disposition.
    Allow,
    /// Drop silently (no storage, no bounce).
    Discard,
    /// Reject with `reason` (Sieve `reject`, RFC 5429). Terminal like
    /// [`FilterAction::Discard`]. A single-recipient, non-null-sender txn is
    /// refused `550 5.7.1 <reason>` at end-of-DATA; a multi-recipient or
    /// null-sender txn drops the recipient silently (no DSN — backscatter-safe).
    Reject { reason: String },
    /// File the message into `mailbox` instead of the disposition default.
    FileInto { mailbox: String },
    /// Forward to `address` (mechanics owned by `mail-forwarding.md`).
    /// `redirect` is the rule's copy mode — `false` keeps the local copy
    /// (`copy`, the default), `true` forwards with no local delivery
    /// (`redirect`): the MTA suppresses the recipient's local placement when
    /// any fired Forward says so (`mail-forwarding.md` § Per-rule "forward to").
    Forward { address: String, redirect: bool },
    /// Vacation auto-reply (Sieve `vacation`, RFC 5230). Non-placement and
    /// non-terminal — fires after delivery, subject to the [`auto_reply_decision`]
    /// loop guard and the `(recipient, sender)` rate limit (`interval_hours`).
    AutoReply {
        subject: String,
        body: String,
        interval_hours: u32,
    },
    /// Deliver normally but add the IMAP keyword/`label`.
    AddLabel { label: String },
}

/// One header as seen at the MTA perimeter (`name` is the field name without the
/// colon; `value` is the unfolded field body).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FilterHeader {
    pub name: String,
    pub value: String,
}

/// One stored filter row to evaluate. Mirrors `fauna_protocol::email::EmailFilter`
/// minus the storage-internal `owner`/`name`/`created_at` (irrelevant to the
/// decision). `priority`/`id` give the deterministic evaluation order
/// (`email.rs` DB: `ORDER BY priority ASC, id ASC`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct StoredFilter {
    pub id: i64,
    pub conditions: Vec<FilterCondition>,
    pub combination: FilterCombination,
    pub action: FilterAction,
    pub priority: i32,
    /// When `false` (the default) a match is **terminal** — first-match-wins
    /// stops here. When `true` (Sieve `continue`) the action is recorded and
    /// evaluation falls through to later rules, so several rules' actions can
    /// apply in order (`smtp-server.md` § Email filter rules — multi-action).
    pub continue_on_match: bool,
}

/// The plaintext-floor context the perimeter evaluates against. Everything here
/// is available to the MTA pre-seal in both storage modes (LBC-2).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FilterContext {
    /// Envelope `MAIL FROM` address (empty for the null sender `<>`).
    pub from: String,
    /// The `Subject` header value (unfolded; empty if absent).
    pub subject: String,
    /// All message headers (`HeaderExists` / `HeaderContains` match against these).
    pub headers: Vec<FilterHeader>,
    /// Combined spam score as a milli-int (`combined_spam_score_milli`).
    pub spam_score_milli: i32,
    /// The decoded message body the `BodyContains` condition matches against —
    /// the `text/plain` + `text/html` parts as the MTA holds them pre-seal,
    /// concatenated and size-capped by the producer (`server.go`, 256 KiB).
    /// Empty when the message has no decodable text body.
    pub body: String,
}

/// One fired filter: which rule matched and the action to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FilterMatch {
    /// The `id` of the matched filter.
    pub filter_id: i64,
    /// The action to apply.
    pub action: FilterAction,
}

/// Evaluate `filters` against `ctx`, returning the **ordered** actions to apply
/// (empty if nothing matched — the caller falls through to spam-disposition
/// placement).
///
/// **Order + `continue` (`smtp-server.md:861` — "Rules are evaluated in order.
/// The first matching rule wins unless the rule is marked `continue`").**
/// Self-contained + deterministic: the engine sorts by `priority ASC, id ASC`
/// (the same order the DB's `list_email_filters` returns), so the caller need
/// not pre-sort. It walks the rules in order, recording each match; a matched
/// rule with [`StoredFilter::continue_on_match`] `== false` is **terminal** and
/// stops the walk (the classic first-match-wins), while a `continue` match falls
/// through to later rules. The returned `Vec` is therefore the matched rules'
/// actions in evaluation order, ending at the first non-`continue` match (or the
/// last rule). Action *composition* (placement override, label accumulation,
/// `Discard` short-circuit) is the caller's job — `smtp-server.md` § Email filter
/// rules spells out the precedence.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn evaluate(filters: Vec<StoredFilter>, ctx: &FilterContext) -> Vec<FilterMatch> {
    let mut filters = filters;
    filters.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.id.cmp(&b.id)));
    let mut matches = Vec::new();
    for f in &filters {
        if filter_matches(f, ctx) {
            matches.push(FilterMatch {
                filter_id: f.id,
                action: f.action.clone(),
            });
            if !f.continue_on_match {
                break;
            }
        }
    }
    matches
}

/// Whether one filter's conditions match `ctx` under its combination. Empty
/// condition set: `All` → true (vacuous), `Any` → false (matches the legacy
/// `rules.iter().all`/`any` semantics).
fn filter_matches(f: &StoredFilter, ctx: &FilterContext) -> bool {
    match f.combination {
        FilterCombination::All => f.conditions.iter().all(|c| condition_matches(c, ctx)),
        FilterCombination::Any => f.conditions.iter().any(|c| condition_matches(c, ctx)),
    }
}

fn condition_matches(c: &FilterCondition, ctx: &FilterContext) -> bool {
    match c {
        FilterCondition::SenderIs { address } => ctx.from.eq_ignore_ascii_case(address),
        FilterCondition::SenderDomain { domain } => ctx
            .from
            .rsplit_once('@')
            .is_some_and(|(_local, d)| d.eq_ignore_ascii_case(domain)),
        FilterCondition::SubjectContains { text } => contains_ci(&ctx.subject, text),
        // v1 body matching: case-insensitive substring over the decoded
        // text/plain + text/html the MTA holds pre-seal (LBC-3 ciphertext ban
        // N/A — this is the plaintext floor). Full Sieve `body` stays deferred.
        FilterCondition::BodyContains { text } => contains_ci(&ctx.body, text),
        FilterCondition::HeaderExists { name } => ctx
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case(name)),
        FilterCondition::HeaderContains { name, value } => ctx
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case(name) && contains_ci(&h.value, value)),
        FilterCondition::SpamScoreAtLeast { milli } => ctx.spam_score_milli >= *milli,
    }
}

/// Case-insensitive substring test (Unicode-lowercased, matching the legacy
/// `store_impl.rs::rule_matches`). An empty needle matches everything.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// The decision for a matched [`FilterAction::AutoReply`] (Sieve `vacation`),
/// evaluated at the MTA perimeter on the plaintext-floor envelope + headers
/// (`smtp-server.md` § Email filter rules — the same storage-mode reason filters
/// evaluate here, not in nest). `Send` means proceed to the rate-limit claim;
/// every other variant is a loop-guard suppression and doubles as a stable
/// metric label (`smtp_inbound_autoreply_suppressed_total{reason}`). The
/// rate-limit itself is a separate, stateful check (`auto_reply_log`) the caller
/// makes after `Send`.
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoReplyGate {
    /// No loop-guard fired — proceed to the rate-limit claim.
    Send,
    /// Envelope sender is null (`MAIL FROM: <>`) — it's a bounce (RFC 3834).
    SuppressNullSender,
    /// `Auto-Submitted` header ≠ `no` — the message is itself automated (RFC 3834).
    SuppressAutoSubmitted,
    /// `List-*` header or `Precedence: bulk|list|junk` — list/bulk mail
    /// (RFC 5230 §4.6, see `mail-mass-mailing.md`).
    SuppressBulk,
    /// Envelope sender is one of our own `local_domains` — don't auto-reply to
    /// our own notifications / postmaster traffic.
    SuppressOwnDomain,
    /// Recipient is not in the message `To`/`Cc` (RFC 5230 §4.4 — guards
    /// bcc/list traffic).
    SuppressNotInRecipients,
}

/// First header value (unfolded) whose name case-insensitively equals `name`.
fn header_value<'a>(headers: &'a [FilterHeader], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

/// Decide whether a matched `AutoReply` action should fire, applying the RFC 5230
/// §4.4/§4.6 + RFC 3834 loop guards in order. Pure + perimeter-evaluable: it
/// reads only the envelope sender, the message headers, the delivery recipient
/// address, and our hosted `local_domains` — all available at the plaintext floor
/// pre-seal in both storage modes. The stateful rate-limit (`auto_reply_log`)
/// is a separate check the caller makes only when this returns
/// [`AutoReplyGate::Send`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn auto_reply_decision(
    envelope_from: &str,
    headers: Vec<FilterHeader>,
    recipient_addr: &str,
    local_domains: Vec<String>,
) -> AutoReplyGate {
    // 1. Null sender — never auto-reply to a bounce (RFC 3834 §2; mirrors the
    //    forward null-sender skip).
    if envelope_from.trim().is_empty() {
        return AutoReplyGate::SuppressNullSender;
    }
    // 2. Auto-Submitted ≠ "no" — the message is itself automated. The field may
    //    carry parameters (`auto-replied; ...`); the keyword is the token before
    //    the first ';'. Anything other than "no" (incl. a bare presence) suppresses.
    if let Some(v) = header_value(&headers, "Auto-Submitted") {
        let keyword = v.split(';').next().unwrap_or("").trim();
        if !keyword.eq_ignore_ascii_case("no") {
            return AutoReplyGate::SuppressAutoSubmitted;
        }
    }
    // 3. List/bulk mail (RFC 5230 §4.6): any `List-*` header, or a
    //    `Precedence:` of bulk/list/junk.
    if headers.iter().any(|h| {
        h.name
            .get(..5)
            .is_some_and(|p| p.eq_ignore_ascii_case("list-"))
    }) {
        return AutoReplyGate::SuppressBulk;
    }
    if let Some(p) = header_value(&headers, "Precedence") {
        let p = p.trim().to_ascii_lowercase();
        if p == "bulk" || p == "list" || p == "junk" {
            return AutoReplyGate::SuppressBulk;
        }
    }
    // 4. Sender is one of our own domains — don't vacation-reply to our own
    //    notifications / postmaster bounces.
    if let Some((_local, dom)) = envelope_from.rsplit_once('@')
        && local_domains.iter().any(|ld| ld.eq_ignore_ascii_case(dom))
    {
        return AutoReplyGate::SuppressOwnDomain;
    }
    // 5. RFC 5230 §4.4 — the delivery recipient must appear in To/Cc (guards
    //    bcc/list traffic). v1: case-insensitive containment of the recipient
    //    address across the combined To + Cc header values.
    let mut to_cc = String::new();
    for h in &headers {
        if h.name.eq_ignore_ascii_case("To") || h.name.eq_ignore_ascii_case("Cc") {
            to_cc.push(' ');
            to_cc.push_str(&h.value);
        }
    }
    if recipient_addr.is_empty()
        || !to_cc
            .to_lowercase()
            .contains(&recipient_addr.to_lowercase())
    {
        return AutoReplyGate::SuppressNotInRecipients;
    }
    AutoReplyGate::Send
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> FilterContext {
        FilterContext {
            from: "Alice@Example.COM".into(),
            subject: "Quarterly REPORT attached".into(),
            headers: vec![
                FilterHeader {
                    name: "List-Id".into(),
                    value: "<golang.example.com>".into(),
                },
                FilterHeader {
                    name: "X-Priority".into(),
                    value: "3".into(),
                },
            ],
            spam_score_milli: 4_500,
            body: "Please find the INVOICE attached.\n<p>Total: $42</p>".into(),
        }
    }

    fn filter(
        id: i64,
        priority: i32,
        c: FilterCombination,
        conds: Vec<FilterCondition>,
        a: FilterAction,
    ) -> StoredFilter {
        StoredFilter {
            id,
            conditions: conds,
            combination: c,
            action: a,
            priority,
            continue_on_match: false,
        }
    }

    /// A filter marked `continue` (falls through to later rules on match).
    fn filter_cont(
        id: i64,
        priority: i32,
        c: FilterCombination,
        conds: Vec<FilterCondition>,
        a: FilterAction,
    ) -> StoredFilter {
        StoredFilter {
            continue_on_match: true,
            ..filter(id, priority, c, conds, a)
        }
    }

    /// The single matched action, asserting exactly one rule fired (the
    /// first-match-wins shape most condition tests assert).
    fn one(matches: Vec<FilterMatch>) -> Option<FilterMatch> {
        assert!(matches.len() <= 1, "expected ≤1 match, got {matches:?}");
        matches.into_iter().next()
    }

    // ── individual conditions ────────────────────────────────────────

    #[test]
    fn sender_is_case_insensitive() {
        assert!(condition_matches(
            &FilterCondition::SenderIs {
                address: "alice@example.com".into()
            },
            &ctx(),
        ));
        assert!(!condition_matches(
            &FilterCondition::SenderIs {
                address: "bob@example.com".into()
            },
            &ctx(),
        ));
    }

    #[test]
    fn sender_domain_after_last_at() {
        assert!(condition_matches(
            &FilterCondition::SenderDomain {
                domain: "EXAMPLE.com".into()
            },
            &ctx(),
        ));
        assert!(!condition_matches(
            &FilterCondition::SenderDomain {
                domain: "example.org".into()
            },
            &ctx(),
        ));
    }

    #[test]
    fn sender_domain_null_sender_never_matches() {
        let mut c = ctx();
        c.from = String::new();
        assert!(!condition_matches(
            &FilterCondition::SenderDomain {
                domain: "example.com".into()
            },
            &c,
        ));
    }

    #[test]
    fn subject_contains_case_insensitive() {
        assert!(condition_matches(
            &FilterCondition::SubjectContains {
                text: "report".into()
            },
            &ctx(),
        ));
        assert!(!condition_matches(
            &FilterCondition::SubjectContains {
                text: "invoice".into()
            },
            &ctx(),
        ));
    }

    #[test]
    fn header_exists_case_insensitive_name() {
        assert!(condition_matches(
            &FilterCondition::HeaderExists {
                name: "list-id".into()
            },
            &ctx(),
        ));
        assert!(!condition_matches(
            &FilterCondition::HeaderExists {
                name: "DKIM-Signature".into()
            },
            &ctx(),
        ));
    }

    #[test]
    fn header_contains_matches_name_and_value() {
        assert!(condition_matches(
            &FilterCondition::HeaderContains {
                name: "List-Id".into(),
                value: "golang".into()
            },
            &ctx(),
        ));
        // right header, wrong value
        assert!(!condition_matches(
            &FilterCondition::HeaderContains {
                name: "List-Id".into(),
                value: "rust".into()
            },
            &ctx(),
        ));
        // right value substring, wrong header
        assert!(!condition_matches(
            &FilterCondition::HeaderContains {
                name: "X-Priority".into(),
                value: "golang".into()
            },
            &ctx(),
        ));
    }

    #[test]
    fn body_contains_case_insensitive() {
        // v1 body matching: case-insensitive substring over the decoded
        // text/plain + text/html the MTA holds pre-seal (ctx().body).
        assert!(condition_matches(
            &FilterCondition::BodyContains {
                text: "invoice".into()
            },
            &ctx(),
        ));
        // matches raw decoded HTML markup (no tag-stripping in v1)
        assert!(condition_matches(
            &FilterCondition::BodyContains {
                text: "<p>total".into()
            },
            &ctx(),
        ));
        assert!(!condition_matches(
            &FilterCondition::BodyContains {
                text: "refund".into()
            },
            &ctx(),
        ));
    }

    #[test]
    fn body_contains_empty_needle_matches() {
        // Mirrors the other *Contains conditions: an empty needle matches
        // everything (`contains_ci`'s documented behavior).
        assert!(condition_matches(
            &FilterCondition::BodyContains {
                text: String::new()
            },
            &ctx(),
        ));
        // …but an empty body with a non-empty needle does not match.
        let mut c = ctx();
        c.body = String::new();
        assert!(!condition_matches(
            &FilterCondition::BodyContains {
                text: "invoice".into()
            },
            &c,
        ));
    }

    #[test]
    fn spam_score_at_least_boundary() {
        // ctx score is 4_500.
        assert!(condition_matches(
            &FilterCondition::SpamScoreAtLeast { milli: 4_500 },
            &ctx(),
        )); // equal → matches (>=)
        assert!(condition_matches(
            &FilterCondition::SpamScoreAtLeast { milli: 4_000 },
            &ctx(),
        ));
        assert!(!condition_matches(
            &FilterCondition::SpamScoreAtLeast { milli: 4_501 },
            &ctx(),
        ));
    }

    // ── combination ──────────────────────────────────────────────────

    #[test]
    fn all_requires_every_condition() {
        let m = one(evaluate(
            vec![filter(
                1,
                0,
                FilterCombination::All,
                vec![
                    FilterCondition::SenderDomain {
                        domain: "example.com".into(),
                    },
                    FilterCondition::SubjectContains {
                        text: "report".into(),
                    },
                ],
                FilterAction::FileInto {
                    mailbox: "Reports".into(),
                },
            )],
            &ctx(),
        ));
        assert_eq!(
            m.unwrap().action,
            FilterAction::FileInto {
                mailbox: "Reports".into()
            }
        );

        // one condition fails → no match under All
        let m = one(evaluate(
            vec![filter(
                1,
                0,
                FilterCombination::All,
                vec![
                    FilterCondition::SenderDomain {
                        domain: "example.com".into(),
                    },
                    FilterCondition::SubjectContains {
                        text: "invoice".into(),
                    },
                ],
                FilterAction::Discard,
            )],
            &ctx(),
        ));
        assert_eq!(m, None);
    }

    #[test]
    fn any_requires_one_condition() {
        let m = one(evaluate(
            vec![filter(
                1,
                0,
                FilterCombination::Any,
                vec![
                    FilterCondition::SubjectContains {
                        text: "invoice".into(),
                    }, // miss
                    FilterCondition::SpamScoreAtLeast { milli: 3_000 }, // hit
                ],
                FilterAction::AddLabel {
                    label: "maybe-spam".into(),
                },
            )],
            &ctx(),
        ));
        assert_eq!(
            m.unwrap().action,
            FilterAction::AddLabel {
                label: "maybe-spam".into()
            }
        );
    }

    #[test]
    fn empty_conditions_all_is_vacuously_true_any_is_false() {
        let all = one(evaluate(
            vec![filter(
                1,
                0,
                FilterCombination::All,
                vec![],
                FilterAction::Discard,
            )],
            &ctx(),
        ));
        assert_eq!(all.unwrap().action, FilterAction::Discard);

        let any = one(evaluate(
            vec![filter(
                1,
                0,
                FilterCombination::Any,
                vec![],
                FilterAction::Discard,
            )],
            &ctx(),
        ));
        assert_eq!(any, None);
    }

    // ── ordering / first-match-wins ──────────────────────────────────

    #[test]
    fn first_match_wins_by_priority_then_id() {
        // Two filters both match; lower priority wins. Pass them out of order to
        // prove the engine sorts internally.
        let lo_priority_high_id = filter(
            99,
            10,
            FilterCombination::All,
            vec![FilterCondition::SenderDomain {
                domain: "example.com".into(),
            }],
            FilterAction::AddLabel {
                label: "low".into(),
            },
        );
        let hi_priority_low_id = filter(
            5,
            1,
            FilterCombination::All,
            vec![FilterCondition::SenderDomain {
                domain: "example.com".into(),
            }],
            FilterAction::FileInto {
                mailbox: "winner".into(),
            },
        );
        let m = one(evaluate(
            vec![lo_priority_high_id, hi_priority_low_id],
            &ctx(),
        ));
        assert_eq!(m.as_ref().unwrap().filter_id, 5);
        assert_eq!(
            m.unwrap().action,
            FilterAction::FileInto {
                mailbox: "winner".into()
            }
        );
    }

    #[test]
    fn ties_on_priority_break_by_id_ascending() {
        let later_id = filter(
            20,
            0,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::AddLabel { label: "b".into() },
        );
        let earlier_id = filter(
            10,
            0,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::AddLabel { label: "a".into() },
        );
        let m = one(evaluate(vec![later_id, earlier_id], &ctx()));
        assert_eq!(m.unwrap().filter_id, 10);
    }

    #[test]
    fn no_filters_no_match() {
        assert!(evaluate(vec![], &ctx()).is_empty());
    }

    #[test]
    fn non_matching_filter_falls_through() {
        let m = one(evaluate(
            vec![filter(
                1,
                0,
                FilterCombination::All,
                vec![FilterCondition::SenderIs {
                    address: "nobody@nowhere.test".into(),
                }],
                FilterAction::Discard,
            )],
            &ctx(),
        ));
        assert_eq!(m, None);
    }

    // ── continue / multi-action ──────────────────────────────────────

    #[test]
    fn default_is_first_match_wins_single_action() {
        // Two matching rules, neither marked continue → only the first (by
        // priority/id) fires; the engine returns exactly one match.
        let a = filter(
            1,
            0,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::FileInto {
                mailbox: "First".into(),
            },
        );
        let b = filter(
            2,
            1,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::FileInto {
                mailbox: "Second".into(),
            },
        );
        let matches = evaluate(vec![a, b], &ctx());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].filter_id, 1);
    }

    #[test]
    fn continue_chains_actions_in_order_until_terminal() {
        // r1 (continue) + r2 (continue) accumulate, r3 is terminal, r4 is never
        // reached even though it matches.
        let r1 = filter_cont(
            1,
            0,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::AddLabel {
                label: "one".into(),
            },
        );
        let r2 = filter_cont(
            2,
            1,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::AddLabel {
                label: "two".into(),
            },
        );
        let r3 = filter(
            3,
            2,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::FileInto {
                mailbox: "Done".into(),
            },
        );
        let r4 = filter(
            4,
            3,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::Discard,
        );
        let matches = evaluate(vec![r1, r2, r3, r4], &ctx());
        let ids: Vec<i64> = matches.iter().map(|m| m.filter_id).collect();
        assert_eq!(ids, vec![1, 2, 3]); // r4 not reached (r3 is terminal)
        assert_eq!(
            matches[2].action,
            FilterAction::FileInto {
                mailbox: "Done".into()
            }
        );
    }

    #[test]
    fn continue_skips_non_matching_rules() {
        // A continue match, then a non-matching rule (skipped), then a terminal
        // match — the skipped rule contributes nothing.
        let r1 = filter_cont(
            1,
            0,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::AddLabel { label: "x".into() },
        );
        let r2 = filter(
            2,
            1,
            FilterCombination::All,
            vec![FilterCondition::SenderIs {
                address: "nobody@nowhere.test".into(),
            }],
            FilterAction::Discard, // no match → skipped
        );
        let r3 = filter(
            3,
            2,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::FileInto {
                mailbox: "End".into(),
            },
        );
        let matches = evaluate(vec![r1, r2, r3], &ctx());
        let ids: Vec<i64> = matches.iter().map(|m| m.filter_id).collect();
        assert_eq!(ids, vec![1, 3]);
    }

    #[test]
    fn all_continue_returns_every_match() {
        // Every matching rule marked continue → all fire, none terminal.
        let r1 = filter_cont(
            1,
            0,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::AddLabel { label: "a".into() },
        );
        let r2 = filter_cont(
            2,
            1,
            FilterCombination::All,
            vec![FilterCondition::SpamScoreAtLeast { milli: 0 }],
            FilterAction::AddLabel { label: "b".into() },
        );
        let matches = evaluate(vec![r1, r2], &ctx());
        assert_eq!(matches.len(), 2);
    }

    // ── combination wire mapping ─────────────────────────────────────

    #[test]
    fn combination_from_wire() {
        assert_eq!(FilterCombination::from_wire("any"), FilterCombination::Any);
        assert_eq!(FilterCombination::from_wire("ANY"), FilterCombination::Any);
        assert_eq!(FilterCombination::from_wire("all"), FilterCombination::All);
        // unknown → All (legacy default)
        assert_eq!(
            FilterCombination::from_wire("garbage"),
            FilterCombination::All
        );
        assert_eq!(FilterCombination::from_wire(""), FilterCombination::All);
    }

    // ── auto_reply_decision (loop guard) ────────────────────────────────────

    fn hdr(name: &str, value: &str) -> FilterHeader {
        FilterHeader {
            name: name.into(),
            value: value.into(),
        }
    }

    fn decide(from: &str, headers: Vec<FilterHeader>, rcpt: &str) -> AutoReplyGate {
        auto_reply_decision(from, headers, rcpt, vec!["fauna.test".into()])
    }

    #[test]
    fn auto_reply_sends_on_a_plain_directly_addressed_message() {
        let h = vec![hdr("To", "Bob <bob@fauna.test>, carol@other.test")];
        assert_eq!(
            decide("alice@elsewhere.test", h, "bob@fauna.test"),
            AutoReplyGate::Send
        );
    }

    #[test]
    fn auto_reply_recipient_match_is_case_insensitive_and_scans_cc() {
        let h = vec![hdr("To", "someone@other.test"), hdr("Cc", "BOB@FAUNA.TEST")];
        assert_eq!(
            decide("alice@elsewhere.test", h, "bob@fauna.test"),
            AutoReplyGate::Send
        );
    }

    #[test]
    fn auto_reply_suppressed_for_null_sender() {
        let h = vec![hdr("To", "bob@fauna.test")];
        assert_eq!(
            decide("", h, "bob@fauna.test"),
            AutoReplyGate::SuppressNullSender
        );
        // Whitespace-only is also "null".
        assert_eq!(
            decide("   ", vec![hdr("To", "bob@fauna.test")], "bob@fauna.test"),
            AutoReplyGate::SuppressNullSender
        );
    }

    #[test]
    fn auto_reply_suppressed_for_auto_submitted() {
        // bare keyword
        let h = vec![
            hdr("To", "bob@fauna.test"),
            hdr("Auto-Submitted", "auto-replied"),
        ];
        assert_eq!(
            decide("alice@elsewhere.test", h, "bob@fauna.test"),
            AutoReplyGate::SuppressAutoSubmitted
        );
        // keyword with parameters
        let h = vec![
            hdr("To", "bob@fauna.test"),
            hdr("Auto-Submitted", "auto-generated; owner=x"),
        ];
        assert_eq!(
            decide("alice@elsewhere.test", h, "bob@fauna.test"),
            AutoReplyGate::SuppressAutoSubmitted
        );
        // "no" is explicitly allowed (RFC 3834) → not suppressed on this axis.
        let h = vec![hdr("To", "bob@fauna.test"), hdr("Auto-Submitted", "no")];
        assert_eq!(
            decide("alice@elsewhere.test", h, "bob@fauna.test"),
            AutoReplyGate::Send
        );
    }

    #[test]
    fn auto_reply_suppressed_for_list_and_bulk() {
        let list = vec![
            hdr("To", "bob@fauna.test"),
            hdr("List-Id", "<l.example.com>"),
        ];
        assert_eq!(
            decide("alice@elsewhere.test", list, "bob@fauna.test"),
            AutoReplyGate::SuppressBulk
        );
        for p in ["bulk", "list", "junk", "BULK"] {
            let h = vec![hdr("To", "bob@fauna.test"), hdr("Precedence", p)];
            assert_eq!(
                decide("alice@elsewhere.test", h, "bob@fauna.test"),
                AutoReplyGate::SuppressBulk,
                "Precedence: {p}"
            );
        }
    }

    #[test]
    fn auto_reply_does_not_panic_on_non_ascii_header_name() {
        // Regression: the `List-*` loop-guard sliced `name[..5]` on a byte length
        // with no char-boundary check, so a header name whose 5th byte fell
        // mid-UTF-8 (here `List€x`, where `€` spans bytes 4–6) panicked the whole
        // mail-bridge process when a recipient with a vacation rule received a
        // crafted inbound header. `List€x` is not a real `List-` header (its 5th
        // char is `€`, not `-`), so the gate must classify past it, not crash.
        let h = vec![hdr("To", "bob@fauna.test"), hdr("List\u{20ac}x", "x")];
        assert_eq!(
            decide("alice@elsewhere.test", h, "bob@fauna.test"),
            AutoReplyGate::Send
        );
        // A genuine `List-*` header still suppresses (multibyte only in the value).
        let h = vec![
            hdr("To", "bob@fauna.test"),
            hdr("List-Id", "<naïve.example.com>"),
        ];
        assert_eq!(
            decide("alice@elsewhere.test", h, "bob@fauna.test"),
            AutoReplyGate::SuppressBulk
        );
    }

    #[test]
    fn auto_reply_suppressed_when_sender_is_one_of_our_domains() {
        let h = vec![hdr("To", "bob@fauna.test")];
        assert_eq!(
            decide("postmaster@fauna.test", h, "bob@fauna.test"),
            AutoReplyGate::SuppressOwnDomain
        );
    }

    #[test]
    fn auto_reply_suppressed_when_recipient_not_in_to_or_cc() {
        // RFC 5230 §4.4 — addressed only via bcc / a list, recipient absent
        // from To/Cc.
        let h = vec![hdr("To", "list@other.test")];
        assert_eq!(
            decide("alice@elsewhere.test", h, "bob@fauna.test"),
            AutoReplyGate::SuppressNotInRecipients
        );
    }
}
