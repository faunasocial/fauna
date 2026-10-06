use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::state::LocalizedText;

/// ⚠ Kept deliberately, though no snapshot state carries it any more.
///
/// This is a **wire** type: `nest_api::InviteRequestResponse.quota` still
/// decodes it, because a nest of any version may send the field and
/// `version-compatibility.md` makes evolution additive — a client removing a
/// field it merely stopped *using* would refuse payloads it can safely ignore.
/// It lost its last snapshot use when `InviteRequestState::Approved` retired
/// (2026-08-12). Do not delete it as "unused".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct InviteQuota {
    pub storage_bytes: u64,
    pub traffic_bytes_per_month: u64,
}

/// Everything an app needs to write the long-term store's pending-invite slot,
/// assembled once here instead of seven times per app.
///
/// This is the seam that replaced `WizardOutcome::InviteSubmitted` (retired
/// 2026-08-12, `onboarding.md` § Wizard exit handling). The pending-review
/// journey no longer *exits* the wizard, so there is no outcome to key the
/// write on; the app reads this at the `wizard_submit_invite_request()` return
/// instead — "the only write moment" (`onboarding.md` § 3 Persistence
/// callouts).
///
/// Two rules live in here rather than in each app, because getting either wrong
/// is silent:
/// - **`nest_url` is `state.nest_url`, never `effective_nest_url()`.** The
///   `provider_base_urls` override retargets HTTP requests only; leaking it here
///   would write a test-cloud URL into a production identity-store record.
/// - **`status_json` is the serialized `InviteRequestState`,** opaque to the
///   app (`onboarding.md` § Long-term store contract: "Don't validate the
///   JSON"). A serialization failure degrades to an empty string so the wizard
///   falls back to `PendingReview` on reseed rather than stranding the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PendingInviteSlot {
    pub nest_url: String,
    pub handle: String,
    pub request_id: String,
    pub status_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ErrorContext {
    Submitting,
    Rechecking,
    Redeeming,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum InviteRequestState {
    Idle,
    Submitting,
    Rechecking,
    // ⚠ There is deliberately no `Approved` variant (retired 2026-08-12,
    // `onboarding.md` § 3). An admin approve *deletes* the request row after
    // creating the account, so no live nest ever serves `status: "approved"`;
    // approval is detected as *admission* instead — the recheck's `NotFound`
    // plus the registered-probe (`onboarding.md` § The pending-invite surface).
    // Do not reintroduce it, and in particular do not inject it in a test: an
    // injected-`Approved` fixture is exactly what kept the variant looking
    // reachable for months while production could never reach it.
    Denied {
        reason: String,
        request_id: String,
    },
    PendingReview {
        request_id: String,
        last_checked_ms: u64,
    },
    Error {
        transient: bool,
        context: ErrorContext,
        cause: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum OobCodeState {
    Idle,
    Verifying,
    Valid {
        invite_id: String,
        /// The guardian's handle if this code carries a supervised
        /// designation (`family-safety.md` § Wire & data shape —
        /// `invite-code-supervised-notice`, rendered before redemption).
        /// `None` = an ordinary code.
        supervised_by: Option<String>,
    },
    Invalid {
        reason: String,
    },
    Error {
        cause: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct InviteRequestSnapshot {
    pub state: InviteRequestState,
    pub message: LocalizedText,
    pub continue_enabled: bool,
    pub recheck_visible: bool,
    pub out_of_band_code_state: OobCodeState,
    /// Localized text for the OOB-code row's status label, derived from
    /// `out_of_band_code_state`. Centralized in shared Rust per
    /// `docs/goal/behavior/onboarding.md` Architectural rule 4 — clients render
    /// `LocalizedText` via their per-platform i18n pipeline; they do NOT
    /// recompute the message from the state variant. Refreshed on every
    /// `invite_request_snapshot()` read so stale values can't leak.
    pub oob_message: LocalizedText,
    /// `invite-request-age-notice` — the store-age round's outcome, shown
    /// before submit/redeem so the user knows what the application carries
    /// (`family-safety.md` § The account age band, D3; a `platform_elements`
    /// entry for android/ios — the five other apps never set a claim and
    /// declare the absence). Derived on every `invite_request_snapshot()` read
    /// from the claim `set_age_claim` holds
    /// ([`fauna_protocol::age::age_notice`]): `None` = the store shared
    /// nothing, so the shell renders no notice. Args are nested keys —
    /// resolve with `resolve_nested`.
    #[serde(default)]
    pub age_notice: Option<LocalizedText>,
}

impl InviteRequestSnapshot {
    pub fn idle() -> Self {
        Self {
            state: InviteRequestState::Idle,
            // Idle disables `invite-request-continue-button`, so idle owes the
            // reason on the row every app already renders for it
            // (`invite-request-status`) — see `oob_message_for`'s Idle arm for
            // the same rule on this page's other half.
            message: LocalizedText::key("onboarding.invite_request.status_idle"),
            continue_enabled: false,
            recheck_visible: false,
            out_of_band_code_state: OobCodeState::Idle,
            oob_message: oob_message_for(&OobCodeState::Idle),
            age_notice: None,
        }
    }
}

impl Default for InviteRequestSnapshot {
    fn default() -> Self {
        Self::idle()
    }
}

/// The key-shaped `cause` a submit refused as already registered carries
/// (`InviteRequestError::AlreadyRegistered`, `login.md` § Errors) — like
/// `invite.error.not_found`, a sentinel [`message_for`] renders as its own
/// sentence rather than wrapping it as `{cause}` prose.
pub const ALREADY_REGISTERED_CAUSE: &str = "invite.error.already_registered";

/// Map an `InviteRequestState` to the admin-flow row's localized status text
/// (`invite-request-status`) per `onboarding.md` Architectural rule 4 — the
/// same derive-on-read rule [`oob_message_for`] already implements for the
/// other half of this page.
///
/// **This exists because `message` was a write-once field.** It was set to the
/// idle copy by [`InviteRequestSnapshot::idle`] and then never written again:
/// neither `wizard_submit_invite_request` nor `recheck_invite_status` nor
/// `invite_error` touched it, so `invite-request-status` sat on "Ask for an
/// invite above…" through Submitting, PendingReview, Denied and every error, on
/// all 7 apps — and a **denied** requester was never told why. The whole
/// `onboarding.invite.*` string family was dead. Only the *injected*-snapshot
/// tier_2 tests looked green, because they set `state` and `message` together,
/// which production never does (`test_pending_invite_journey.py` is what found
/// it: the real deny journey read back the idle line).
///
/// Total by construction: every state this page can reach has a string. It
/// returned `Option` until 2026-08-12 purely to carry one `None` arm for the
/// dead `Approved` variant, whose copy needed pre-formatted byte-size args that
/// shared Rust cannot produce without hard-coding English (rule 4 forbids it).
/// Both retired together, so the caller no longer has an "unreachable" branch
/// to get wrong.
pub fn message_for(state: &InviteRequestState) -> LocalizedText {
    match state {
        // Deliberately NOT `onboarding.invite.idle`. Both keys are idle copy;
        // this is the one `idle()` has always rendered, written for
        // `ui/README.md` § Copy comprehensibility rule 5 (it names both ways
        // forward, because Idle is where `invite-request-continue-button` is
        // dead). Keeping it means this fix changes no already-correct screen.
        InviteRequestState::Idle => LocalizedText::key("onboarding.invite_request.status_idle"),
        InviteRequestState::Submitting => LocalizedText::key("onboarding.invite.submitting"),
        InviteRequestState::Rechecking => LocalizedText::key("onboarding.invite.rechecking"),
        // ⚠ The ratified reword ("…you'll continue automatically once they
        // respond") still waits, now on the LAST THREE surfaces: tui, linux,
        // web and android poll as of 2026-08-12, windows / macOS / iOS do not.
        // The string is shared by all 7 apps, so shipping the promise before
        // those three have a timer would be a lie on exactly the apps that
        // cannot keep it. The copy follows the mechanism — flip this key the
        // moment their timers land (`onboarding.md` § Implementation status
        // today).
        InviteRequestState::PendingReview { .. } => {
            LocalizedText::key("onboarding.invite.pending_review")
        }
        InviteRequestState::Denied { reason, .. } => {
            let mut args = HashMap::new();
            args.insert("reason".into(), reason.clone());
            LocalizedText {
                key: "onboarding.invite.denied".into(),
                args,
            }
        }
        InviteRequestState::Error {
            transient, cause, ..
        } => {
            // `resolve_not_found_recheck` is the one producer that emits a
            // key-shaped sentinel instead of nest prose, and it is the ratified
            // terminal of this page's journey ("probe refuted admission" —
            // `onboarding.md` § 3 Persistence callouts), so it gets its own
            // string rather than being wrapped in a generic error frame.
            if cause == "invite.error.not_found" {
                LocalizedText::key("onboarding.invite.error.not_found")
            } else if cause == ALREADY_REGISTERED_CAUSE {
                // The submit's second sentinel: a key this nest already holds
                // (for a user on this page, a suspended one). Its own honest
                // sentence, never the raw wire code in the terminal frame.
                LocalizedText::key("onboarding.invite.error.already_registered")
            } else {
                let mut args = HashMap::new();
                args.insert("cause".into(), cause.clone());
                // `onboarding.invite.error.{closed,rate_limited}` stay unrouted
                // on purpose: the machine collapses both nest refusals into
                // `Error { transient: false }` carrying the nest's OWN message
                // as `cause`, and rendering that message beats replacing it
                // with a generic sentence. Routing them would need a
                // discriminator the state does not carry — a wider change than
                // this page owes, and one that would throw away nest text.
                LocalizedText {
                    key: if *transient {
                        "onboarding.invite.error.transient".into()
                    } else {
                        "onboarding.invite.error.terminal".into()
                    },
                    args,
                }
            }
        }
    }
}

/// Map an `OobCodeState` to its localized status-row text per
/// `docs/goal/behavior/onboarding.md` Architectural rule 4. The keys live under
/// `onboarding.invite_request.oob_*` in `i18n/strings/en.yaml`. Clients
/// resolve `oob_message.key` through their per-platform i18n table (or the
/// shared Rust resolver `LocalizedText::resolve`).
pub fn oob_message_for(state: &OobCodeState) -> LocalizedText {
    match state {
        // NOT `default()`. Idle is the state where `invite-code-check-button`
        // is dead (the field is empty), so it is precisely the state that owes
        // a reason — a blank status row beside a DIM control explains nothing
        // (`ui/README.md` § Copy comprehensibility rule 5). The silent-arm
        // shape shared with `dns_status_text_key`'s old `NotReady`.
        OobCodeState::Idle => LocalizedText::key("onboarding.invite_request.oob_idle"),
        OobCodeState::Verifying => LocalizedText {
            key: "common.verifying".into(),
            args: HashMap::new(),
        },
        OobCodeState::Valid { .. } => LocalizedText {
            key: "onboarding.invite_request.oob_valid".into(),
            args: HashMap::new(),
        },
        OobCodeState::Invalid { reason } => {
            let mut args = HashMap::new();
            args.insert("reason".into(), reason.clone());
            LocalizedText {
                key: "onboarding.invite_request.oob_invalid".into(),
                args,
            }
        }
        OobCodeState::Error { cause } => {
            let mut args = HashMap::new();
            args.insert("cause".into(), cause.clone());
            LocalizedText {
                key: "onboarding.invite_request.oob_error".into(),
                args,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_has_continue_disabled() {
        assert!(!InviteRequestSnapshot::idle().continue_enabled);
    }

    /// Both idle rows explain their dead control rather than painting blank —
    /// and they say different things, because the page's two halves ask for
    /// different next acts (`ui/README.md` § Copy comprehensibility rule 5 +
    /// Q2).
    #[test]
    fn both_idle_status_rows_explain_their_dead_controls() {
        let s = InviteRequestSnapshot::idle();
        assert!(!s.continue_enabled, "precondition: Continue is dead");
        assert_eq!(s.message.key, "onboarding.invite_request.status_idle");
        assert_eq!(s.oob_message.key, "onboarding.invite_request.oob_idle");
        assert_ne!(
            s.message.key, s.oob_message.key,
            "two conditions want two messages, not one generic line"
        );
    }
    /// Every state a live nest can put this page in owes its OWN line. The
    /// regression this pins: `message` used to be write-once, so a requester
    /// read the idle copy while Submitting, while PendingReview, and — worst —
    /// while **Denied**, where the reason is the entire point of the row.
    #[test]
    fn every_reachable_state_gets_its_own_message() {
        let cases = [
            (
                InviteRequestState::Idle,
                "onboarding.invite_request.status_idle",
            ),
            (
                InviteRequestState::Submitting,
                "onboarding.invite.submitting",
            ),
            (
                InviteRequestState::Rechecking,
                "onboarding.invite.rechecking",
            ),
            (
                InviteRequestState::PendingReview {
                    request_id: "R".into(),
                    last_checked_ms: 1,
                },
                "onboarding.invite.pending_review",
            ),
            (
                InviteRequestState::Denied {
                    reason: "not now".into(),
                    request_id: "R".into(),
                },
                "onboarding.invite.denied",
            ),
        ];
        let mut keys = std::collections::HashSet::new();
        for (state, expected) in &cases {
            let msg = message_for(state);
            assert_eq!(&msg.key, expected, "{state:?}");
            assert!(
                keys.insert(msg.key.clone()),
                "{state:?} reuses another state's line — the write-once bug in a new shape"
            );
        }
    }

    /// The denial reason travels in the args, not in prose the client builds
    /// (Architectural rule 4). Without this the row renders "Request denied: "
    /// and the requester learns nothing.
    #[test]
    fn denied_carries_its_reason_as_an_arg() {
        let msg = message_for(&InviteRequestState::Denied {
            reason: "we are not accepting requests right now".into(),
            request_id: "R".into(),
        });
        assert_eq!(
            msg.args.get("reason").map(String::as_str),
            Some("we are not accepting requests right now")
        );
    }

    /// A terminal refusal must not read "Try again" — it will not fix itself —
    /// and the ratified `invite.error.not_found` sentinel keeps its own string
    /// rather than being wrapped as opaque `{cause}` prose.
    #[test]
    fn error_states_split_transient_from_terminal() {
        let transient = message_for(&InviteRequestState::Error {
            transient: true,
            context: ErrorContext::Submitting,
            cause: "timeout".into(),
        });
        assert_eq!(transient.key, "onboarding.invite.error.transient");
        assert_eq!(
            transient.args.get("cause").map(String::as_str),
            Some("timeout")
        );

        let terminal = message_for(&InviteRequestState::Error {
            transient: false,
            context: ErrorContext::Submitting,
            cause: "this nest is closed".into(),
        });
        assert_eq!(terminal.key, "onboarding.invite.error.terminal");
        assert_ne!(
            terminal.key, transient.key,
            "a terminal refusal that reads like a retry is the bug this splits"
        );

        // The one producer that emits a key-shaped sentinel rather than nest
        // prose (`machine::resolve_not_found_recheck`).
        let not_found = message_for(&InviteRequestState::Error {
            transient: false,
            context: ErrorContext::Rechecking,
            cause: "invite.error.not_found".into(),
        });
        assert_eq!(not_found.key, "onboarding.invite.error.not_found");
        assert!(
            not_found.args.is_empty(),
            "the sentinel is the message, not an argument to one"
        );

        // The submit's already-registered refusal (a suspended key holder on
        // the "Use a different nest" route, `login.md` § Errors) — the second
        // key-shaped sentinel, rendered as its own honest sentence rather than
        // the raw `fauna.account.actor_exists` code in a generic frame.
        let already = message_for(&InviteRequestState::Error {
            transient: false,
            context: ErrorContext::Submitting,
            cause: ALREADY_REGISTERED_CAUSE.into(),
        });
        assert_eq!(already.key, "onboarding.invite.error.already_registered");
        assert!(already.args.is_empty());
    }

    /// `message_for` is TOTAL — the property that replaced the dead `Approved`
    /// variant's `None` arm (retired 2026-08-12).
    ///
    /// This is the pin that makes the write-once class un-reintroducible rather
    /// than merely fixed: the old bug was a state the read path had no string
    /// for, and the old `Option` return is exactly what let such a state exist
    /// silently. Adding a variant to `InviteRequestState` now fails to compile
    /// here (the match below is exhaustive with no wildcard), and a variant
    /// wired to an empty key fails the assert. Do not "fix" a future red by
    /// adding a `_ =>` arm — that recreates the hole.
    #[test]
    fn every_state_derives_a_non_empty_message() {
        let all = [
            InviteRequestState::Idle,
            InviteRequestState::Submitting,
            InviteRequestState::Rechecking,
            InviteRequestState::Denied {
                reason: "r".into(),
                request_id: "R".into(),
            },
            InviteRequestState::PendingReview {
                request_id: "R".into(),
                last_checked_ms: 1,
            },
            InviteRequestState::Error {
                transient: true,
                context: ErrorContext::Submitting,
                cause: "c".into(),
            },
        ];
        for state in &all {
            // Exhaustiveness guard: this match has no wildcard, so a new
            // variant breaks the build here and gets a string on purpose.
            match state {
                InviteRequestState::Idle
                | InviteRequestState::Submitting
                | InviteRequestState::Rechecking
                | InviteRequestState::Denied { .. }
                | InviteRequestState::PendingReview { .. }
                | InviteRequestState::Error { .. } => {}
            }
            assert!(
                !message_for(state).key.is_empty(),
                "{state:?} derives an empty key — the write-once bug in a new shape"
            );
        }
    }

    /// A `PendingReview` slot is what a relaunch actually hydrates from, so its
    /// round-trip is the one this page's persistence depends on.
    #[test]
    fn pending_review_round_trips() {
        let s = InviteRequestState::PendingReview {
            request_id: "R".into(),
            last_checked_ms: 12_345,
        };
        let raw = serde_json::to_string(&s).unwrap();
        let s2: InviteRequestState = serde_json::from_str(&raw).unwrap();
        assert_eq!(s, s2);
    }

    /// The unparseable-`status_json` degrade, pinned with a corrupt slot (the
    /// retired `Approved` spelling).
    ///
    /// `status_json` in the long-term store's pending-invite slot is a
    /// serialized `InviteRequestState`; an empty or corrupt blob (here the
    /// retired `Approved` spelling) does not parse.
    ///
    /// That is the *correct* outcome, not a loss: `OnboardingMachine::seed_pending_invite` degrades an
    /// unparseable slot to `PendingReview` (`unwrap_or`), which resumes polling,
    /// and the poll's registered-probe then settles what actually happened. The
    /// old behavior — rendering `Approved` with a Continue button whose redeem
    /// was refused as `ActorAlreadyRegistered` — was a dead end. So the degrade
    /// strictly improves on it.
    ///
    /// (In practice no such blob should exist: the slot is only ever written
    /// from `PendingReview`, and no live nest serves `"approved"`. This pins the
    /// degradation anyway, because "should not exist" is not a guarantee.)
    #[test]
    fn an_unparseable_approved_blob_does_not_parse_and_that_is_safe() {
        let retired = r#"{"Approved":{"quota":{"storage_bytes":1,"traffic_bytes_per_month":2},"request_id":"R"}}"#;
        assert!(
            serde_json::from_str::<InviteRequestState>(retired).is_err(),
            "the variant is retired"
        );
        // The degradation `seed_pending_invite` relies on: an unparseable slot
        // must fall back, never panic or drop the request.
        let recovered = serde_json::from_str::<InviteRequestState>(retired).unwrap_or(
            InviteRequestState::PendingReview {
                request_id: "R".into(),
                last_checked_ms: 0,
            },
        );
        assert!(matches!(
            recovered,
            InviteRequestState::PendingReview { .. }
        ));
    }
}
