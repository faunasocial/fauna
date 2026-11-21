//! The onboarding wizard's draft-validity predicates (`docs/goal/ui/README.md`
//! § Offer only what is possible, rule 4 — *the form case* — and rule 6).
//!
//! Inside a form that can succeed, the draft itself is a precondition of the
//! submit. Each function here answers [`Possibility`] from what has been typed
//! — *possible*, or *blocked* with the reason key of the field that is missing
//! or malformed — so every app paints its submit from one answer and none
//! re-derives it from `!is_empty()`. The submit asks the same question again:
//! the restore and the lock run their local checks through the very function
//! the predicate wraps ([`recovery_entry_draft`], [`emergency_lockout_draft`]),
//! so what the page offers and what the action refuses cannot disagree.
//!
//! The reasons are the strings the submits already answer with on
//! `error-message`; a draft predicate adds no new copy.

use fauna_client_recovery::ParsedKit;
use fauna_core::identity_qr::{ImportedIdentity, parse_import_input};
use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

use crate::machine::{EmergencyLockoutOutcome, RecoveryEntryOutcome, parse_handle_domain};

/// `import-submit-button` on `identity_import`: the paste must be an identity
/// secret in one of the import grammars (a bare 64-hex key, the
/// `fauna://identity` link, the colon form). Blank and malformed answer the
/// same `invalid_secret` line the submit refuses with.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn identity_import_possibility(pasted: String) -> Possibility {
    match parse_import_input(&pasted) {
        Some(_) => Possibility::Possible,
        None => Possibility::blocked(LocalizedText::key(
            "onboarding.identity_import.invalid_secret",
        )),
    }
}

/// The restore's local checks, in the form's field order: the phrase must be
/// a recovery kit, and an account must be named — typed, or carried by the
/// kit — as `user@domain`. On success: the parsed kit, the account and its
/// domain. Shared by [`recovery_entry_possibility`] and
/// `OnboardingMachine::submit_recovery_entry`, so both refuse alike.
pub(crate) fn recovery_entry_draft(
    phrase: &str,
    account: &str,
) -> Result<(ParsedKit, String, String), RecoveryEntryOutcome> {
    let kit =
        fauna_client_recovery::parse_kit(phrase).map_err(|_| RecoveryEntryOutcome::InvalidKit)?;
    // The typed field wins over the payload: a user who names an account is
    // correcting or supplying what the phrase could not say.
    let typed = account.trim();
    let account = if typed.is_empty() {
        kit.handle.clone()
    } else {
        Some(typed.to_string())
    };
    let account = account.ok_or(RecoveryEntryOutcome::AccountNeeded)?;
    // Only a handle's `@domain` can locate a nest, and the ceremony is
    // pre-identity — there is no session to ask.
    let domain = parse_handle_domain(&account).ok_or(RecoveryEntryOutcome::AccountMalformed)?;
    Ok((kit, account, domain))
}

/// `recovery-entry-submit-button` on `recovery_entry`: possible once the
/// phrase is a recovery kit and an account is named, else blocked with the
/// `onboarding.recovery_entry.*` line the restore itself refuses with.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn recovery_entry_possibility(phrase: String, account: String) -> Possibility {
    match recovery_entry_draft(&phrase, &account) {
        Ok(_) => Possibility::Possible,
        Err(outcome) => blocked_by(outcome.message()),
    }
}

/// The signed-out lock's local checks, in the form's field order: the paste
/// must be an identity secret, an account must be named (typed, or carried by
/// the link) as `user@domain`, and the confirm field must hold exactly
/// [`fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD`]. On success: the
/// parsed identity, the account and its domain. Shared by
/// [`emergency_lockout_possibility`] and
/// `OnboardingMachine::submit_emergency_lockout`.
pub(crate) fn emergency_lockout_draft(
    secret: &str,
    account: &str,
    confirm_word: &str,
) -> Result<(ImportedIdentity, String, String), EmergencyLockoutOutcome> {
    let imported = parse_import_input(secret).ok_or(EmergencyLockoutOutcome::InvalidSecret)?;
    let typed = account.trim();
    let account = if typed.is_empty() {
        imported.handle.clone()
    } else {
        Some(typed.to_string())
    };
    let account = account.ok_or(EmergencyLockoutOutcome::AccountNeeded)?;
    let domain = parse_handle_domain(&account).ok_or(EmergencyLockoutOutcome::AccountMalformed)?;
    if confirm_word != fauna_client_recovery::ceremony::LOCKOUT_CONFIRM_WORD {
        return Err(EmergencyLockoutOutcome::WrongConfirmWord);
    }
    Ok((imported, account, domain))
}

/// `sessions-lockout-button` on `emergency_lockout_entry`: possible once the
/// secret, the account and the confirm word all hold, else blocked with the
/// line the lock itself refuses with.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn emergency_lockout_possibility(
    secret: String,
    account: String,
    confirm_word: String,
) -> Possibility {
    match emergency_lockout_draft(&secret, &account, &confirm_word) {
        Ok(_) => Possibility::Possible,
        Err(outcome) => Possibility::blocked(outcome.message("")),
    }
}

/// The claim code's draft: a code typed. The nest normalizes the code on both
/// sides, so nothing past blankness is judged here (`onboarding.md` § 3a).
/// Blocked with the page's own idle line, which already names the step.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn claim_code_draft_possibility(code: String) -> Possibility {
    blank_blocks(&code, "onboarding.claim_code.idle")
}

/// `invite-code-check-button`'s draft: a code typed. Blocked with the
/// out-of-band row's idle line, which already names the step.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn invite_code_draft_possibility(code: String) -> Possibility {
    blank_blocks(&code, "onboarding.invite_request.oob_idle")
}

/// `handle-check-button`'s draft: a handle typed. Its form is the check's own
/// first phase (`format_invalid` is an outcome the page reports), so only
/// blankness blocks. Blocked with the check's idle line.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn handle_draft_possibility(handle: String) -> Possibility {
    blank_blocks(&handle, "onboarding.handle_check.idle")
}

fn blank_blocks(draft: &str, key: &str) -> Possibility {
    if draft.trim().is_empty() {
        Possibility::blocked(LocalizedText::key(key))
    } else {
        Possibility::Possible
    }
}

fn blocked_by(reason: Option<LocalizedText>) -> Possibility {
    // Every local refusal speaks (`RecoveryEntryOutcome::message` is `None`
    // only for the two routing outcomes, which a draft never produces).
    Possibility::from_blocked_reason(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built at run time: a 64-hex literal in a shipped file reads as a key to
    // the publish secret scan.
    static SECRET: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| "01".repeat(32));

    fn reason(p: &Possibility) -> &str {
        p.reason().map(|r| r.key.as_str()).unwrap_or("<possible>")
    }

    #[test]
    fn identity_import_blocks_blank_and_malformed_pastes() {
        for draft in ["", "   ", "not a key", &SECRET[..63]] {
            assert_eq!(
                reason(&identity_import_possibility(draft.into())),
                "onboarding.identity_import.invalid_secret",
                "{draft:?}"
            );
        }
        assert!(identity_import_possibility(SECRET.clone()).is_possible());
    }

    #[test]
    fn recovery_entry_names_the_first_missing_or_malformed_field() {
        let p = |phrase: &str, account: &str| {
            reason(&recovery_entry_possibility(phrase.into(), account.into())).to_string()
        };
        assert_eq!(
            p("", "alice@example.com"),
            "onboarding.recovery_entry.invalid_kit"
        );
        assert_eq!(p("nonsense", ""), "onboarding.recovery_entry.invalid_kit");
        // A bare 64-hex code names no account.
        assert_eq!(p(&SECRET, ""), "onboarding.recovery_entry.account_needed");
        assert_eq!(p(&SECRET, "  "), "onboarding.recovery_entry.account_needed");
        assert_eq!(
            p(&SECRET, "alice"),
            "onboarding.recovery_entry.account_malformed"
        );
        assert_eq!(p(&SECRET, "alice@example.com"), "<possible>");
    }

    #[test]
    fn emergency_lockout_checks_secret_then_account_then_the_word() {
        let p = |s: &str, a: &str, c: &str| {
            reason(&emergency_lockout_possibility(s.into(), a.into(), c.into())).to_string()
        };
        assert_eq!(p("", "", ""), "onboarding.emergency_lockout.invalid_secret");
        assert_eq!(
            p("zz", "a@b.example", "LOCK"),
            "onboarding.emergency_lockout.invalid_secret"
        );
        assert_eq!(
            p(&SECRET, "", "LOCK"),
            "onboarding.emergency_lockout.account_needed"
        );
        assert_eq!(
            p(&SECRET, "alice", "LOCK"),
            "onboarding.emergency_lockout.account_malformed"
        );
        assert_eq!(
            p(&SECRET, "alice@example.com", ""),
            "sessions.lockout_wrong_word"
        );
        assert_eq!(
            p(&SECRET, "alice@example.com", "lock"),
            "sessions.lockout_wrong_word"
        );
        assert_eq!(p(&SECRET, "alice@example.com", "LOCK"), "<possible>");
    }

    #[test]
    fn the_code_and_handle_drafts_block_only_a_blank() {
        for (f, key) in [
            (
                claim_code_draft_possibility as fn(String) -> Possibility,
                "onboarding.claim_code.idle",
            ),
            (
                invite_code_draft_possibility,
                "onboarding.invite_request.oob_idle",
            ),
            (handle_draft_possibility, "onboarding.handle_check.idle"),
        ] {
            assert_eq!(reason(&f(String::new())), key);
            assert_eq!(reason(&f(" \t".into())), key);
            assert!(f("x".into()).is_possible(), "{key}");
        }
    }

    /// Every reason a draft predicate names resolves in the shared string
    /// table — a key that does not is painted as its raw key.
    #[test]
    fn every_draft_reason_resolves() {
        let answers = [
            identity_import_possibility(String::new()),
            recovery_entry_possibility(String::new(), String::new()),
            recovery_entry_possibility(SECRET.clone(), String::new()),
            recovery_entry_possibility(SECRET.clone(), "alice".into()),
            emergency_lockout_possibility(String::new(), String::new(), String::new()),
            emergency_lockout_possibility(SECRET.clone(), String::new(), String::new()),
            emergency_lockout_possibility(SECRET.clone(), "alice".into(), String::new()),
            emergency_lockout_possibility(SECRET.clone(), "a@b.example".into(), String::new()),
            claim_code_draft_possibility(String::new()),
            invite_code_draft_possibility(String::new()),
            handle_draft_possibility(String::new()),
        ];
        for answer in answers {
            let r = answer.reason().expect("each draft above is blocked");
            assert!(
                fauna_i18n::strings::lookup(&r.key).is_some(),
                "{} does not resolve",
                r.key
            );
        }
    }
}
