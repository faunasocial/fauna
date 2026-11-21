//! The `nest_provisioning` page's DNS plan (`provisioning-dns-plan`): what
//! the run will keep, add, change and remove in the domain's zone, read when
//! the page opens. Owner: `docs/goal/behavior/onboarding-provisioning.md` § 6
//! *The DNS plan*. The rows' text is resolved here so every app renders the
//! same sentence and none re-derives it.

use std::collections::HashMap;

use fauna_core::localized::LocalizedText;
use fauna_provisioning::dns_plan::{DnsPlanAction, DnsPlanRow};
use serde::{Deserialize, Serialize};

/// One rendered row, keyed `<name>|<type>` for `provisioning-dns-plan-row[…]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsPlanRowView {
    /// The row's key in `provisioning-dns-plan-row[<key>]`: `<name>|<type>`.
    pub key: String,
    pub action: DnsPlanAction,
    pub text: LocalizedText,
}

/// The region's whole state. `visible == false` → the app renders no region
/// (the manual path, a bought domain's fresh zone, before the page opened).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsPlanSnapshot {
    pub visible: bool,
    /// The heading line — or, while reading or after a failed read, the line
    /// that says so (then `rows` is empty).
    pub heading: LocalizedText,
    pub rows: Vec<DnsPlanRowView>,
}

const K: &str = "onboarding.nest_provisioning.";

fn text(key: &str, args: &[(&str, String)]) -> LocalizedText {
    LocalizedText {
        key: format!("{K}{key}"),
        args: args
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect::<HashMap<_, _>>(),
    }
}

impl DnsPlanSnapshot {
    pub fn hidden() -> Self {
        Self {
            visible: false,
            heading: LocalizedText::default(),
            rows: Vec::new(),
        }
    }

    pub fn loading(domain: &str) -> Self {
        Self {
            visible: true,
            heading: text("dns_plan_loading", &[("domain", domain.to_string())]),
            rows: Vec::new(),
        }
    }

    pub fn read_failed(domain: &str, cause: &str) -> Self {
        Self {
            visible: true,
            heading: text(
                "dns_plan_read_failed",
                &[("domain", domain.to_string()), ("cause", cause.to_string())],
            ),
            rows: Vec::new(),
        }
    }

    /// The plan's rows, rendered. `new_server` is the resolved text standing
    /// for an address not known before the server exists — passed in as a
    /// nested key would not be resolved by every app's template renderer.
    pub fn ready(domain: &str, rows: &[DnsPlanRow], new_server: &str) -> Self {
        let rows = rows
            .iter()
            .map(|r| {
                let value = r.desired.clone().unwrap_or_else(|| new_server.to_string());
                let old = r.current.join(", ");
                let base = [
                    ("name", r.name.clone()),
                    ("record_type", r.record_type.clone()),
                ];
                let (key, extra): (&str, Vec<(&str, String)>) = match r.action {
                    DnsPlanAction::Keep => ("dns_plan_row_keep", vec![("value", value)]),
                    DnsPlanAction::Add => ("dns_plan_row_add", vec![("value", value)]),
                    DnsPlanAction::Change => {
                        ("dns_plan_row_change", vec![("old", old), ("value", value)])
                    }
                    DnsPlanAction::Remove => ("dns_plan_row_remove", vec![("old", old)]),
                };
                let args: Vec<(&str, String)> = base.into_iter().chain(extra).collect();
                DnsPlanRowView {
                    key: format!("{}|{}", r.name, r.record_type),
                    action: r.action,
                    text: text(key, &args),
                }
            })
            .collect();
        Self {
            visible: true,
            heading: text("dns_plan_heading", &[("domain", domain.to_string())]),
            rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        n: &str,
        t: &str,
        action: DnsPlanAction,
        cur: &[&str],
        want: Option<&str>,
    ) -> DnsPlanRow {
        DnsPlanRow {
            name: n.into(),
            record_type: t.into(),
            action,
            current: cur.iter().map(|s| s.to_string()).collect(),
            desired: want.map(str::to_string),
        }
    }

    /// Every key the region emits resolves, and a change names the old value
    /// and the new one.
    #[test]
    fn every_line_resolves_and_a_change_names_old_and_new() {
        let rows = [
            row("@", "A", DnsPlanAction::Change, &["9.9.9.9"], None),
            row(
                "@",
                "MX",
                DnsPlanAction::Keep,
                &["mail.x.test"],
                Some("mail.x.test"),
            ),
            row("mail", "A", DnsPlanAction::Add, &[], None),
            row("@", "AAAA", DnsPlanAction::Remove, &["2001:db8::1"], None),
        ];
        let snap = DnsPlanSnapshot::ready("x.test", &rows, "NEW");
        let loading = DnsPlanSnapshot::loading("x.test");
        let failed = DnsPlanSnapshot::read_failed("x.test", "boom");
        let all = std::iter::once(&snap.heading)
            .chain(snap.rows.iter().map(|r| &r.text))
            .chain([&loading.heading, &failed.heading]);
        for t in all {
            assert!(
                fauna_i18n::strings::lookup(&t.key).is_some(),
                "unresolved {}",
                t.key
            );
        }
        let change = &snap.rows[0];
        assert_eq!(change.key, "@|A");
        assert_eq!(
            change.text.args.get("old").map(String::as_str),
            Some("9.9.9.9")
        );
        assert_eq!(
            change.text.args.get("value").map(String::as_str),
            Some("NEW")
        );
        assert_eq!(snap.rows[3].action, DnsPlanAction::Remove);
        assert!(!DnsPlanSnapshot::hidden().visible);
    }
}
