//! The admin DNS page's `Possibility` answers for the DNS half of the page
//! (`docs/goal/ui/README.md` § Offer only what is possible, rules 2–7; the
//! ruling is `docs/goal/behavior/dns-management.md` § Errors & edge cases).
//!
//! Each answers from the already-read [`DnsSnapshot`] (`None` = the read has
//! not resolved, never possible — rule 5) and, for the add-credential form, the
//! draft typed so far. Every app paints the control from the answer; the action
//! asks the same answer first and refuses with the same reason —
//! [`DnsManagementMachine`](crate::DnsManagementMachine)'s own `SetMode` refusal
//! included, so what the page offers and what the machine accepts agree.
//!
//! The existing bool reads ([`DnsSnapshot::all_domains_managed`],
//! [`DomainView::is_managed`](crate::DomainView::is_managed)) stay for the FFI
//! and wasm readers; these are their siblings with the reason.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

use crate::{DnsSnapshot, PendingCertIssue, zone_covers};

/// Blocked reason: managed mode needs a held credential covering the domain.
pub const MODE_NEEDS_CREDENTIAL: &str = "admin.dns.mode_needs_credential";
/// Blocked reason: the master switch has no active domain to act on.
pub const MANAGE_ALL_NEEDS_DOMAIN: &str = "admin.dns.manage_all_needs_domain";
/// Blocked reason: the master switch's "on" needs every active domain covered.
pub const MANAGE_ALL_NEEDS_CREDENTIAL: &str = "admin.dns.manage_all_needs_credential";
/// Blocked reason: the add-credential form has no provider picked.
pub const CREDENTIAL_PROVIDER_REQUIRED: &str = "admin.dns.credential_provider_required";
/// Blocked reason: an add-credential field is blank.
pub const CREDENTIAL_FIELD_REQUIRED: &str = "admin.dns.credential_field_required";
/// Blocked reason: this domain already has a manual certificate order waiting.
pub const CERT_ISSUE_PENDING: &str = "admin.dns.cert.issue_pending";

fn blocked(key: &str) -> Possibility {
    Possibility::blocked(LocalizedText::key(key))
}

/// Whether a held credential's zone list covers `domain` — the one coverage
/// test the managed opt-in, its paint and its refusal share.
pub fn credential_covers<'a>(zones: impl IntoIterator<Item = &'a str>, domain: &str) -> bool {
    zones.into_iter().any(|z| zone_covers(z, domain))
}

impl DnsSnapshot {
    /// Whether some held credential covers `domain`.
    pub fn covers(&self, domain: &str) -> bool {
        self.credentials
            .iter()
            .any(|c| credential_covers(c.zones.iter().map(String::as_str), domain))
    }

    /// `admin-dns-domain-mode` on `domain`, for the switch to `managed`:
    /// opting out is always possible; opting in needs a covering credential —
    /// the add-credential step is what unblocks it.
    pub fn set_mode_possibility(&self, domain: &str, managed: bool) -> Possibility {
        if managed && !self.covers(domain) {
            return blocked(MODE_NEEDS_CREDENTIAL);
        }
        Possibility::Possible
    }
}

/// `admin-dns-manage-all-toggle`, for the switch to `managed`, over the
/// authoritative active-domain names. Not possible before both reads resolve;
/// with no active domain there is nothing to switch (add a domain first); the
/// "on" needs every active domain covered by a held credential.
pub fn manage_all_possibility(
    dns: Option<&DnsSnapshot>,
    active_domains: Option<&[String]>,
    managed: bool,
) -> Possibility {
    let (Some(dns), Some(active)) = (dns, active_domains) else {
        return Possibility::once_read(false);
    };
    if active.is_empty() {
        return blocked(MANAGE_ALL_NEEDS_DOMAIN);
    }
    if managed && !active.iter().all(|d| dns.covers(d)) {
        return blocked(MANAGE_ALL_NEEDS_CREDENTIAL);
    }
    Possibility::Possible
}

/// `admin-dns-add-credential-submit-button`: a provider picked and every one
/// of its DNS fields typed. `fields` is each field id the provider asks for,
/// with the value typed into it (blank when untouched).
pub fn credential_draft_possibility(provider_picked: bool, fields: &[(&str, &str)]) -> Possibility {
    if !provider_picked {
        return blocked(CREDENTIAL_PROVIDER_REQUIRED);
    }
    if fields.is_empty() || fields.iter().any(|(_, v)| v.trim().is_empty()) {
        return blocked(CREDENTIAL_FIELD_REQUIRED);
    }
    Possibility::Possible
}

/// `admin-dns-cert-issue-button` on `domain`: not while this domain's manual
/// order waits for the admin to finish or cancel it (one order at a time).
pub fn cert_issue_possibility(pending: Option<&PendingCertIssue>, domain: &str) -> Possibility {
    if pending.is_some_and(|p| p.domain == domain) {
        return blocked(CERT_ISSUE_PENDING);
    }
    Possibility::Possible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CredentialSummary;

    fn key(p: &Possibility) -> Option<&str> {
        p.reason().map(|r| r.key.as_str())
    }

    fn snapshot(zones: &[&str]) -> DnsSnapshot {
        let mut s = DnsSnapshot::empty();
        if !zones.is_empty() {
            s.credentials.push(CredentialSummary {
                provider_id: "hetzner".into(),
                zones: zones.iter().map(|z| z.to_string()).collect(),
                label: "Hetzner".into(),
            });
        }
        s
    }

    #[test]
    fn managed_mode_needs_a_covering_credential_and_manual_never_does() {
        let none = snapshot(&[]);
        assert_eq!(
            key(&none.set_mode_possibility("a.test", true)),
            Some(MODE_NEEDS_CREDENTIAL)
        );
        assert!(none.set_mode_possibility("a.test", false).is_possible());
        let held = snapshot(&["a.test"]);
        assert!(held.set_mode_possibility("a.test", true).is_possible());
        assert!(held.set_mode_possibility("mail.a.test", true).is_possible());
        assert_eq!(
            key(&held.set_mode_possibility("b.test", true)),
            Some(MODE_NEEDS_CREDENTIAL)
        );
    }

    #[test]
    fn the_master_switch_waits_for_both_reads_then_needs_domains_and_coverage() {
        let names = vec!["a.test".to_string(), "b.test".to_string()];
        let held = snapshot(&["a.test"]);
        assert_eq!(
            key(&manage_all_possibility(None, Some(&names), true)),
            Some("common.still_loading")
        );
        assert_eq!(
            key(&manage_all_possibility(Some(&held), None, true)),
            Some("common.still_loading")
        );
        assert_eq!(
            key(&manage_all_possibility(Some(&held), Some(&[]), false)),
            Some(MANAGE_ALL_NEEDS_DOMAIN)
        );
        assert_eq!(
            key(&manage_all_possibility(Some(&held), Some(&names), true)),
            Some(MANAGE_ALL_NEEDS_CREDENTIAL)
        );
        assert!(manage_all_possibility(Some(&held), Some(&names), false).is_possible());
        assert!(manage_all_possibility(Some(&held), Some(&names[..1]), true).is_possible());
    }

    #[test]
    fn the_credential_form_needs_a_provider_then_every_field() {
        assert_eq!(
            key(&credential_draft_possibility(false, &[])),
            Some(CREDENTIAL_PROVIDER_REQUIRED)
        );
        assert_eq!(
            key(&credential_draft_possibility(true, &[])),
            Some(CREDENTIAL_FIELD_REQUIRED)
        );
        assert_eq!(
            key(&credential_draft_possibility(
                true,
                &[("api-key", "k"), ("secret-api-key", " ")]
            )),
            Some(CREDENTIAL_FIELD_REQUIRED)
        );
        assert!(credential_draft_possibility(true, &[("api-token", "t")]).is_possible());
    }

    #[test]
    fn cert_issue_waits_only_on_this_domains_pending_order() {
        let pending = PendingCertIssue {
            domain: "a.test".into(),
            challenges: Vec::new(),
        };
        assert_eq!(
            key(&cert_issue_possibility(Some(&pending), "a.test")),
            Some(CERT_ISSUE_PENDING)
        );
        assert!(cert_issue_possibility(Some(&pending), "b.test").is_possible());
        assert!(cert_issue_possibility(None, "a.test").is_possible());
    }
}
