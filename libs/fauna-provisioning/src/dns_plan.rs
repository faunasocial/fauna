//! The DNS plan: what a provisioning run will keep, add, change and remove in
//! a zone that may already hold records — most often an earlier box's, left
//! behind when that box was deleted.
//!
//! Owner: `docs/goal/behavior/onboarding-provisioning.md` § 6 *The DNS plan*.
//! Pure: the caller reads the zone's current records (one `find_records` per
//! `(name, type)` [`lookups`] names) and hands them in; the same function both
//! renders the page's list before the run and drives the Dns step's
//! convergence during it, so what is shown and what is done cannot drift.
//!
//! **What counts as the same record.** At a name the run publishes, every `A`
//! and every `MX` competes with the run's; a `TXT` competes only when it is of
//! the same kind (`v=spf1`, `v=DMARC1`), so a site-verification `TXT` beside
//! the SPF is never touched. An `AAAA` at a name the run points at the box is
//! removed: the run publishes none, and a stale one sends IPv6 visitors to the
//! old address.

use serde::{Deserialize, Serialize};

use crate::dns::DnsRecord;

/// What the run does to one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum DnsPlanAction {
    /// Already holds exactly the desired value.
    Keep,
    /// Absent; the run creates it.
    Add,
    /// Present with another value; the run deletes the old and creates the new.
    Change,
    /// An `AAAA` at a name the run points at the box; the run deletes it.
    Remove,
}

/// One row of the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsPlanRow {
    /// Zone-relative name as the run publishes it (`@`, `mail`, `_dmarc`).
    pub name: String,
    pub record_type: String,
    pub action: DnsPlanAction,
    /// The competing values found in the zone (empty for `Add`).
    pub current: Vec<String>,
    /// The value the run publishes; `None` when it is the new box's address
    /// and the box does not exist yet. `None` for `Remove`.
    pub desired: Option<String>,
}

/// The desired value standing for "the new box's address" before it exists.
/// [`plan`] treats a desired `A` with this value as unknown.
pub const UNKNOWN_ADDRESS: &str = "";

/// The `(name, type)` pairs whose current records [`plan`] needs.
pub fn lookups(desired: &[DnsRecord]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |n: &str, t: &str| {
        let k = (n.to_string(), t.to_string());
        if !out.contains(&k) {
            out.push(k);
        }
    };
    for r in desired {
        push(&r.name, &r.record_type);
        if r.record_type == "A" {
            push(&r.name, "AAAA");
        }
    }
    out
}

/// Compute the plan. `current(name, type)` returns the zone's records at that
/// pair (every pair [`lookups`] names; an unread pair reads as empty).
pub fn plan(
    desired: &[DnsRecord],
    current: impl Fn(&str, &str) -> Vec<DnsRecord>,
) -> Vec<DnsPlanRow> {
    let mut rows = Vec::new();
    for want in desired {
        let competing: Vec<String> = current(&want.name, &want.record_type)
            .into_iter()
            .filter(|r| competes(want, r))
            .map(|r| r.value)
            .collect();
        let known = !(want.record_type == "A" && want.value == UNKNOWN_ADDRESS);
        let action = if competing.is_empty() {
            DnsPlanAction::Add
        } else if known
            && competing.len() == 1
            && same_value(&want.record_type, &competing[0], &want.value)
        {
            DnsPlanAction::Keep
        } else {
            DnsPlanAction::Change
        };
        rows.push(DnsPlanRow {
            name: want.name.clone(),
            record_type: want.record_type.clone(),
            action,
            current: competing,
            desired: known.then(|| want.value.clone()),
        });
    }
    let mut a_names: Vec<&str> = desired
        .iter()
        .filter(|r| r.record_type == "A")
        .map(|r| r.name.as_str())
        .collect();
    a_names.dedup();
    for name in a_names {
        let v6: Vec<String> = current(name, "AAAA").into_iter().map(|r| r.value).collect();
        if !v6.is_empty() {
            rows.push(DnsPlanRow {
                name: name.to_string(),
                record_type: "AAAA".into(),
                action: DnsPlanAction::Remove,
                current: v6,
                desired: None,
            });
        }
    }
    rows
}

/// The plan's rows named zone-relative (`@`, `mail`, `_dmarc`) — the one form
/// a person reads whatever DNS provider they connected, the form the admin DNS
/// page names records in (onboarding-provisioning.md § 6 *The DNS plan*). The
/// records the Dns step writes keep the provider's own form (fully qualified
/// for Cloudflare, [`crate::dns::DnsProvider::record_names_relative_to_zone`]);
/// only what is shown and keyed is normalized. A name already zone-relative is
/// left as it is.
pub fn named_in_zone(rows: Vec<DnsPlanRow>, zone: &str) -> Vec<DnsPlanRow> {
    let zone = zone.trim_end_matches('.').to_ascii_lowercase();
    rows.into_iter()
        .map(|mut r| {
            let name = r.name.trim_end_matches('.').to_ascii_lowercase();
            r.name = crate::orchestrator::dns_record_name(&name, &zone);
            r
        })
        .collect()
}

/// The values the Dns step deletes for one row before creating the desired
/// one: every competing value that is not the desired value.
pub fn stale_values(row: &DnsPlanRow) -> Vec<String> {
    match row.action {
        DnsPlanAction::Keep | DnsPlanAction::Add => Vec::new(),
        DnsPlanAction::Remove => row.current.clone(),
        DnsPlanAction::Change => row
            .current
            .iter()
            .filter(|v| match &row.desired {
                Some(d) => !same_value(&row.record_type, v, d),
                None => true,
            })
            .cloned()
            .collect(),
    }
}

/// Whether the Dns step creates the desired record for this row: an `Add`, or
/// a `Change` whose desired value is not already among the competing ones (a
/// duplicate beside the right value only deletes).
pub fn needs_create(row: &DnsPlanRow) -> bool {
    match row.action {
        DnsPlanAction::Add => true,
        DnsPlanAction::Change => match &row.desired {
            Some(d) => !row
                .current
                .iter()
                .any(|v| same_value(&row.record_type, v, d)),
            None => true,
        },
        DnsPlanAction::Keep | DnsPlanAction::Remove => false,
    }
}

fn competes(want: &DnsRecord, have: &DnsRecord) -> bool {
    if want.record_type != "TXT" {
        return true;
    }
    match txt_kind(&want.value) {
        Some(kind) => txt_kind(&have.value) == Some(kind),
        // A TXT of no known kind competes only with its own value.
        None => have.value == want.value,
    }
}

fn txt_kind(v: &str) -> Option<&'static str> {
    let v = v.trim().trim_matches('"').to_ascii_lowercase();
    if v.starts_with("v=spf1") {
        Some("spf1")
    } else if v.starts_with("v=dmarc1") {
        Some("dmarc1")
    } else {
        None
    }
}

fn same_value(record_type: &str, a: &str, b: &str) -> bool {
    norm(record_type, a) == norm(record_type, b)
}

fn norm(record_type: &str, v: &str) -> String {
    let v = v.trim();
    match record_type {
        "TXT" => v.trim_matches('"').to_string(),
        "MX" => {
            // Some providers fold the priority into the value ("10 mail.x.").
            let host = v
                .split_once(char::is_whitespace)
                .filter(|(p, _)| p.chars().all(|c| c.is_ascii_digit()))
                .map(|(_, h)| h.trim())
                .unwrap_or(v);
            host.trim_end_matches('.').to_ascii_lowercase()
        }
        _ => v.trim_end_matches('.').to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(t: &str, n: &str, v: &str) -> DnsRecord {
        DnsRecord {
            record_type: t.into(),
            name: n.into(),
            value: v.into(),
            ttl: 300,
            priority: None,
        }
    }

    fn desired(ip: &str) -> Vec<DnsRecord> {
        vec![
            rec("A", "@", ip),
            rec("A", "mail", ip),
            rec("MX", "@", "mail.x.test"),
            rec("TXT", "@", "v=spf1 mx -all"),
            rec("TXT", "_dmarc", "v=DMARC1; p=quarantine"),
        ]
    }

    fn zone(recs: Vec<DnsRecord>) -> impl Fn(&str, &str) -> Vec<DnsRecord> {
        move |n, t| {
            recs.iter()
                .filter(|r| r.name == n && r.record_type == t)
                .cloned()
                .collect()
        }
    }

    fn action(rows: &[DnsPlanRow], n: &str, t: &str) -> DnsPlanAction {
        rows.iter()
            .find(|r| r.name == n && r.record_type == t)
            .unwrap_or_else(|| panic!("no row {n} {t}: {rows:#?}"))
            .action
    }

    #[test]
    fn empty_zone_adds_everything() {
        let rows = plan(&desired("1.2.3.4"), zone(vec![]));
        assert!(
            rows.iter().all(|r| r.action == DnsPlanAction::Add),
            "{rows:#?}"
        );
        assert_eq!(rows.len(), 5);
    }

    /// The deleted-box case: the old box's records are all there, pointing at
    /// the old address. The address records change; the mail records that
    /// already say the right thing are kept.
    #[test]
    fn an_old_boxs_records_change_only_where_they_are_wrong() {
        let old = zone(vec![
            rec("A", "@", "9.9.9.9"),
            rec("A", "mail", "9.9.9.9"),
            rec("MX", "@", "10 mail.x.test."),
            rec("TXT", "@", "v=spf1 mx -all"),
            rec("TXT", "_dmarc", "v=DMARC1; p=quarantine"),
            rec("AAAA", "@", "2001:db8::1"),
        ]);
        let rows = plan(&desired("1.2.3.4"), old);
        assert_eq!(action(&rows, "@", "A"), DnsPlanAction::Change);
        assert_eq!(action(&rows, "mail", "A"), DnsPlanAction::Change);
        assert_eq!(action(&rows, "@", "MX"), DnsPlanAction::Keep);
        assert_eq!(action(&rows, "@", "TXT"), DnsPlanAction::Keep);
        assert_eq!(action(&rows, "_dmarc", "TXT"), DnsPlanAction::Keep);
        assert_eq!(action(&rows, "@", "AAAA"), DnsPlanAction::Remove);
        let a = rows
            .iter()
            .find(|r| r.name == "@" && r.record_type == "A")
            .unwrap();
        assert_eq!(stale_values(a), vec!["9.9.9.9".to_string()]);
    }

    #[test]
    fn unknown_address_is_never_keep() {
        let rows = plan(
            &desired(UNKNOWN_ADDRESS),
            zone(vec![rec("A", "@", "9.9.9.9")]),
        );
        let a = rows
            .iter()
            .find(|r| r.name == "@" && r.record_type == "A")
            .unwrap();
        assert_eq!(a.action, DnsPlanAction::Change);
        assert_eq!(a.desired, None);
        assert_eq!(action(&rows, "mail", "A"), DnsPlanAction::Add);
    }

    #[test]
    fn other_txt_records_are_never_touched() {
        let rows = plan(
            &desired("1.2.3.4"),
            zone(vec![
                rec("TXT", "@", "google-site-verification=abc"),
                rec("TXT", "@", "v=spf1 include:other.example -all"),
            ]),
        );
        let spf = rows
            .iter()
            .find(|r| r.name == "@" && r.record_type == "TXT")
            .unwrap();
        assert_eq!(spf.action, DnsPlanAction::Change);
        assert_eq!(
            spf.current,
            vec!["v=spf1 include:other.example -all".to_string()]
        );
        assert_eq!(stale_values(spf), spf.current);
    }

    #[test]
    fn a_duplicate_beside_the_right_value_is_a_change_that_deletes_only_the_wrong_one() {
        let rows = plan(
            &desired("1.2.3.4"),
            zone(vec![rec("A", "@", "1.2.3.4"), rec("A", "@", "9.9.9.9")]),
        );
        let a = rows
            .iter()
            .find(|r| r.name == "@" && r.record_type == "A")
            .unwrap();
        assert_eq!(a.action, DnsPlanAction::Change);
        assert_eq!(stale_values(a), vec!["9.9.9.9".to_string()]);
        assert!(!needs_create(a));
    }

    #[test]
    fn lookups_cover_every_desired_pair_and_aaaa_at_a_names() {
        let l = lookups(&desired("1.2.3.4"));
        assert!(l.contains(&("@".into(), "AAAA".into())));
        assert!(l.contains(&("mail".into(), "AAAA".into())));
        assert!(l.contains(&("_dmarc".into(), "TXT".into())));
        assert_eq!(l.iter().filter(|p| p.0 == "@" && p.1 == "TXT").count(), 1);
    }
}
