//! The admin DNS page's domain-row answers and form-validity predicates
//! (`docs/goal/ui/README.md` § Offer only what is possible, rules 2, 4 and 5;
//! the ruling is `docs/goal/behavior/dns-management.md` § Errors & edge cases).
//!
//! Each answers [`Possibility`] from the already-read [`LocalDomainsSnapshot`]
//! (`None` = the domains read has not resolved yet, which is never possible —
//! rule 5) and, for a form, from the draft typed so far. Every app paints the
//! control from the answer and the action asks it again and refuses with the
//! same reason; the nest stays the authority and re-validates everything.
//!
//! The existing bool hints on the snapshot (`rename_available`,
//! `adding_first_domain`) stay as they are for FFI and wasm readers; these are
//! their `Possibility` siblings, with the reason the bool cannot carry.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

use crate::local_domains::{LocalDomainView, LocalDomainsSnapshot};

/// Blocked reason: the add-domain box is blank.
pub const DOMAIN_REQUIRED: &str = "admin.dns.domain_required";
/// Blocked reason: the add-domain box does not hold a domain name.
pub const DOMAIN_MALFORMED: &str = "admin.dns.domain_malformed";
/// Blocked reason: the typed domain is already an active domain.
pub const DOMAIN_DUPLICATE: &str = "admin.dns.domain_duplicate";
/// Blocked reason: the primary domain is renamed away, never removed.
pub const REMOVE_PRIMARY: &str = "admin.dns.remove_primary_blocked";
/// Blocked reason: a rename needs a second domain to promote.
pub const RENAME_NEEDS_SECOND_DOMAIN: &str = "admin.dns.rename.needs_second_domain";
/// Blocked reason: one rename at a time.
pub const RENAME_IN_PROGRESS: &str = "admin.dns.rename.in_progress";
/// Blocked reason: the rename sheet's new-primary pick is not an active
/// non-primary domain.
pub const RENAME_TARGET_REQUIRED: &str = "admin.dns.rename.target_required";
/// Blocked reason: a day count outside `[RENAME_DAYS_MIN, RENAME_DAYS_MAX]`.
pub const RENAME_DAYS_INVALID: &str = "admin.dns.rename.days_invalid";

/// The smallest grace window, and the smallest extension, the nest accepts
/// (`fauna.bridges.start_primary_rename` / `extend_primary_rename_grace`).
pub const RENAME_DAYS_MIN: i64 = 1;
/// The largest grace window, and the largest single extension, the nest accepts.
pub const RENAME_DAYS_MAX: i64 = 30;

fn blocked(key: &str) -> Possibility {
    Possibility::blocked(LocalizedText::key(key))
}

/// Whether `s` is a domain name an admin can add: two or more dot-separated
/// labels of `[A-Za-z0-9-]`, each 1–63 bytes with no leading or trailing
/// hyphen, at most 253 bytes in all (RFC 1035 § 2.3.1's preferred syntax, with
/// the digit-first labels RFC 1123 allows). A dotless name is refused: a mail
/// domain nobody outside the box can resolve is a typo, not a choice.
pub fn is_domain_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = s.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// `admin-dns-add-domain-submit-button`: the domains read resolved (the
/// first-domain warning depends on it — rule 5), a domain typed, well formed,
/// and not already active.
pub fn add_domain_possibility(domains: Option<&LocalDomainsSnapshot>, draft: &str) -> Possibility {
    let Some(domains) = domains else {
        return Possibility::once_read(false);
    };
    let draft = draft.trim();
    if draft.is_empty() {
        return blocked(DOMAIN_REQUIRED);
    }
    if !is_domain_name(draft) {
        return blocked(DOMAIN_MALFORMED);
    }
    if domains
        .active
        .iter()
        .any(|d| d.domain.eq_ignore_ascii_case(draft))
    {
        return blocked(DOMAIN_DUPLICATE);
    }
    Possibility::Possible
}

/// `admin-dns-domain-remove-button` on `row`: every row but the primary, which
/// is renamed onto another domain instead.
pub fn remove_possibility(row: &LocalDomainView) -> Possibility {
    if row.is_primary {
        return blocked(REMOVE_PRIMARY);
    }
    Possibility::Possible
}

/// `admin-dns-domain-rename-button` (the primary row): no rename in flight and
/// a second domain to promote — the add-domain step is what unblocks it.
pub fn rename_possibility(domains: &LocalDomainsSnapshot) -> Possibility {
    if domains.active_rename.is_some() {
        return blocked(RENAME_IN_PROGRESS);
    }
    if !domains.rename_available {
        return blocked(RENAME_NEEDS_SECOND_DOMAIN);
    }
    Possibility::Possible
}

/// `admin-dns-domain-promote-button` (a non-primary row): no rename in flight.
pub fn promote_possibility(domains: &LocalDomainsSnapshot) -> Possibility {
    if domains.active_rename.is_some() {
        return blocked(RENAME_IN_PROGRESS);
    }
    Possibility::Possible
}

/// A day-count draft: blank takes the nest's default (`None`), otherwise a
/// whole number in `[RENAME_DAYS_MIN, RENAME_DAYS_MAX]`. `Err` is never
/// silently replaced by a default.
pub fn parse_rename_days(draft: &str) -> Result<Option<i64>, Possibility> {
    let draft = draft.trim();
    if draft.is_empty() {
        return Ok(None);
    }
    match draft.parse::<i64>() {
        Ok(days) if (RENAME_DAYS_MIN..=RENAME_DAYS_MAX).contains(&days) => Ok(Some(days)),
        _ => Err(blocked(RENAME_DAYS_INVALID)),
    }
}

/// `admin-dns-rename-submit-button`: no rename in flight, the pick an active
/// non-primary domain, and the grace draft blank or a valid day count.
pub fn rename_draft_possibility(
    domains: Option<&LocalDomainsSnapshot>,
    target: &str,
    grace_days: &str,
) -> Possibility {
    let Some(domains) = domains else {
        return Possibility::once_read(false);
    };
    if domains.active_rename.is_some() {
        return blocked(RENAME_IN_PROGRESS);
    }
    if !domains
        .active
        .iter()
        .any(|d| !d.is_primary && d.domain == target)
    {
        return blocked(RENAME_TARGET_REQUIRED);
    }
    match parse_rename_days(grace_days) {
        Ok(_) => Possibility::Possible,
        Err(reason) => reason,
    }
}

/// `admin-dns-rename-extend-button`: the extend draft blank (the default
/// extension) or a valid day count.
pub fn extend_draft_possibility(days: &str) -> Possibility {
    match parse_rename_days(days) {
        Ok(_) => Possibility::Possible,
        Err(reason) => reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_domains::LocalDomainStatus;

    fn key(p: &Possibility) -> Option<&str> {
        p.reason().map(|r| r.key.as_str())
    }

    fn row(domain: &str, is_primary: bool) -> LocalDomainView {
        LocalDomainView {
            domain_id: domain.as_bytes().to_vec(),
            domain: domain.to_string(),
            is_primary,
            mta_sts_mode: "enforce".to_string(),
            mta_sts_cert_mode: "expand_primary".to_string(),
            mta_sts_max_age_seconds: 86400,
            spf_record: String::new(),
            dkim_selector: None,
            dkim_rotation_due: false,
            dkim_selector_activated_at: None,
            catch_all_actor_id: None,
            catch_all_cleared_by_succession_at: None,
            role_address_overrides: Vec::new(),
            dmarc_policy: crate::local_domains::DomainDmarcPolicy::Reject,
            added_at: 0,
            removed_at: None,
        }
    }

    fn snapshot(active: Vec<LocalDomainView>) -> LocalDomainsSnapshot {
        let rename_available =
            active.iter().any(|d| d.is_primary) && active.iter().any(|d| !d.is_primary);
        LocalDomainsSnapshot {
            adding_first_domain: active.is_empty(),
            active,
            soft_deleted: Vec::new(),
            status: LocalDomainStatus::Idle,
            error: None,
            last_add_skipped: false,
            active_rename: None,
            rename_available,
            remove_confirm_domain: None,
        }
    }

    #[test]
    fn a_domain_name_is_dotted_labels_of_letters_digits_and_inner_hyphens() {
        for good in [
            "example.com",
            "mail-1.sub.example.test",
            "1password.test",
            "xn--bcher-kva.example",
        ] {
            assert!(is_domain_name(good), "{good}");
        }
        for bad in [
            "",
            "localhost",
            "a..b",
            ".a.b",
            "a.b.",
            "-a.b",
            "a-.b",
            "a_b.c",
            "a b.c",
            "a/b.c",
        ] {
            assert!(!is_domain_name(bad), "{bad}");
        }
        assert!(!is_domain_name(&format!("{}.com", "a".repeat(64))));
    }

    #[test]
    fn add_domain_waits_for_the_read_then_checks_the_draft() {
        assert_eq!(
            key(&add_domain_possibility(None, "a.test")),
            Some("common.still_loading")
        );
        let s = snapshot(vec![row("Main.test", true)]);
        assert_eq!(
            key(&add_domain_possibility(Some(&s), "  ")),
            Some(DOMAIN_REQUIRED)
        );
        assert_eq!(
            key(&add_domain_possibility(Some(&s), "nodot")),
            Some(DOMAIN_MALFORMED)
        );
        assert_eq!(
            key(&add_domain_possibility(Some(&s), "main.TEST")),
            Some(DOMAIN_DUPLICATE)
        );
        assert!(add_domain_possibility(Some(&s), " other.test ").is_possible());
    }

    #[test]
    fn the_primary_row_is_renamed_never_removed() {
        assert_eq!(
            key(&remove_possibility(&row("a.test", true))),
            Some(REMOVE_PRIMARY)
        );
        assert!(remove_possibility(&row("b.test", false)).is_possible());
    }

    #[test]
    fn rename_needs_a_second_domain_and_no_rename_in_flight() {
        let one = snapshot(vec![row("a.test", true)]);
        assert_eq!(
            key(&rename_possibility(&one)),
            Some(RENAME_NEEDS_SECOND_DOMAIN)
        );
        let two = snapshot(vec![row("a.test", true), row("b.test", false)]);
        assert!(rename_possibility(&two).is_possible());
        assert!(promote_possibility(&two).is_possible());
        let mut busy = two.clone();
        busy.active_rename = Some(crate::primary_domain_rename::PrimaryDomainRenameView {
            rename_id: vec![1],
            state: "pre_flip".to_string(),
            old_primary_domain: "a.test".to_string(),
            new_primary_domain: "b.test".to_string(),
            started_at: 0,
            grace_days: 7,
            grace_ends_at: None,
            ready_to_complete_at: None,
            is_post_flip_active: false,
            is_pre_flip: true,
            can_complete: false,
            can_force_complete: false,
            can_extend: false,
            can_abort: true,
        });
        assert_eq!(key(&rename_possibility(&busy)), Some(RENAME_IN_PROGRESS));
        assert_eq!(key(&promote_possibility(&busy)), Some(RENAME_IN_PROGRESS));
    }

    #[test]
    fn day_counts_are_blank_or_in_range_never_silently_defaulted() {
        assert_eq!(parse_rename_days(" "), Ok(None));
        assert_eq!(parse_rename_days("1"), Ok(Some(1)));
        assert_eq!(parse_rename_days("30"), Ok(Some(30)));
        for bad in ["0", "31", "-2", "seven", "7.5"] {
            assert_eq!(
                parse_rename_days(bad).map_err(|p| key(&p).map(str::to_string)),
                Err(Some(RENAME_DAYS_INVALID.to_string())),
                "{bad}"
            );
        }
        assert!(extend_draft_possibility("").is_possible());
        assert_eq!(
            key(&extend_draft_possibility("x")),
            Some(RENAME_DAYS_INVALID)
        );
    }

    #[test]
    fn the_rename_sheet_needs_a_non_primary_pick_and_valid_grace() {
        assert_eq!(
            key(&rename_draft_possibility(None, "b.test", "")),
            Some("common.still_loading")
        );
        let s = snapshot(vec![row("a.test", true), row("b.test", false)]);
        assert_eq!(
            key(&rename_draft_possibility(Some(&s), "", "")),
            Some(RENAME_TARGET_REQUIRED)
        );
        assert_eq!(
            key(&rename_draft_possibility(Some(&s), "a.test", "")),
            Some(RENAME_TARGET_REQUIRED)
        );
        assert_eq!(
            key(&rename_draft_possibility(Some(&s), "b.test", "99")),
            Some(RENAME_DAYS_INVALID)
        );
        assert!(rename_draft_possibility(Some(&s), "b.test", "").is_possible());
        assert!(rename_draft_possibility(Some(&s), "b.test", "14").is_possible());
    }
}
