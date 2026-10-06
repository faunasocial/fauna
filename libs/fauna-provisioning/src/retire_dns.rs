//! The DNS-cleanup plan for retiring a box — the inverse of the orchestrator's
//! `build_records` (`docs/goal/behavior/nest-retirement.md` § DNS cleanup —
//! scope and order).
//!
//! **Value-scoped, never a name sweep.** A record is planned for removal only
//! when it *points at the box being destroyed*: an `A`/`AAAA` whose value is
//! that box's address, an `MX`/`SRV` whose target is that domain's mail host,
//! the floor `TLSA` under that mail host. A person's zone is not Fauna's to
//! tidy — deleting every `A` at the apex because a nest once lived there would
//! take out whatever they pointed it at since.
//!
//! **What stays** — SPF, DMARC, MTA-STS and TLSRPT `TXT` — is listed for the
//! person to remove by hand instead. Those live at names shared with non-Fauna
//! records, carry no pointer to the box, and are harmless stale, which is why
//! that list is built even on the happy path.
//!
//! **When no usable DNS credential holds the zone**, nothing can be removed
//! *for* the person, so the by-hand list is the whole output — and then it
//! carries the value-scoped records too, ahead of the `TXT`: those are the
//! ones that point at an address the provider is about to hand to a stranger,
//! and the `TXT` are not. See [`RetireDnsPlan::by_hand_only`].
//!
//! **A box serves several domains** (§ DNS cleanup — several domains). The
//! attributed domain is the one whose `mail.` is the deployment's single MX
//! host; every other local domain the nest served published its own apex
//! `A`/`AAAA` → the box and an `MX` / DAV `SRV` → that same mail host. So a
//! retire plans one [`plan_dns_cleanup`] for the attributed domain and one
//! [`plan_secondary_dns_cleanup`] per other verified domain, and the machine
//! [`RetireDnsPlan::append`]s them — the same value-scoped rules, domain by
//! domain, so the second domain's apex never outlives the box either.
//!
//! PTR needs no cleanup: it dies with the address.

use fauna_mail::dns::per_domain::mail_host;

/// A record the run will delete, scoped to an exact value.
///
/// `value` is not advisory: the deletion is a `find_records(name, type)`
/// pre-flight followed by a `delete_record` for the entries whose value
/// matches, so a record another service put at the same name survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRemoval {
    pub name: String,
    pub record_type: String,
    pub value: String,
}

/// Why a record is being left behind, so the view can say so rather than
/// listing bare names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftoverReason {
    /// Lives at a name shared with non-Fauna records and carries no pointer to
    /// the box, so removing it automatically could take out something else.
    SharedName,
    /// Points at the box being destroyed, and no usable DNS credential holds
    /// the zone — so the run cannot remove it, but leaving it unlisted is the
    /// dangling-DNS takeover itself.
    PointsAtBox,
}

/// A record left in place and listed for the person to remove by hand.
///
/// `value` identifies *which* entry at that name to delete, so a record the
/// person has since repointed is visibly not the one meant. Empty where the
/// value is not ours to reconstruct — the `TXT` rrsets, and the `TLSA`, both
/// keyed to a name rather than a value (`PlannedRemoval` does the same).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftoverRecord {
    pub name: String,
    pub record_type: String,
    pub value: String,
    pub reason: LeftoverReason,
}

/// What the DNS step will do, and what it deliberately will not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetireDnsPlan {
    pub removals: Vec<PlannedRemoval>,
    pub leftovers: Vec<LeftoverRecord>,
}

impl RetireDnsPlan {
    /// The by-hand list alone — what the view shows when no usable DNS
    /// credential holds the domain's zone, so nothing can be removed
    /// automatically (§ DNS cleanup).
    ///
    /// It is **not** just the `TXT`. Every record [`plan_dns_cleanup`] would
    /// have removed comes first, each with the value that identifies it: those
    /// are the `A`/`AAAA`, `MX`, `SRV` and `TLSA` still pointing at a box whose
    /// address the provider will re-issue, and a person told only about the
    /// four harmless-stale `TXT` walks into exactly the dangling-DNS takeover
    /// the DNS-first order exists to prevent.
    ///
    /// Takes `plan_dns_cleanup`'s own `(domain, ipv4, ipv6)` on purpose: the
    /// list is *derived* from that plan, so anything the plan learns to remove
    /// this list learns to name, with no second change here.
    pub fn leftovers_only(domain: &str, ipv4: Option<&str>, ipv6: Option<&str>) -> Self {
        plan_dns_cleanup(domain, ipv4, ipv6).by_hand_only()
    }

    /// This plan's by-hand form: every removal becomes a
    /// [`LeftoverReason::PointsAtBox`] leftover, listed ahead of the `TXT`.
    /// What [`Self::leftovers_only`] is for the attributed domain, for any
    /// domain's plan — a secondary domain in a zone no credential holds takes
    /// [`plan_secondary_dns_cleanup`] through this.
    pub fn by_hand_only(self) -> Self {
        let would_have_removed = self.removals.into_iter().map(|r| LeftoverRecord {
            name: r.name,
            record_type: r.record_type,
            value: r.value,
            reason: LeftoverReason::PointsAtBox,
        });
        Self {
            removals: Vec::new(),
            leftovers: would_have_removed.chain(self.leftovers).collect(),
        }
    }

    /// Concatenate another domain's plan onto this one — how a multi-domain
    /// box's plan is assembled, one domain at a time.
    pub fn append(&mut self, other: Self) {
        self.removals.extend(other.removals);
        self.leftovers.extend(other.leftovers);
    }
}

/// The `TXT` records Fauna publishes that are deliberately never auto-removed.
fn leftovers_for(domain: &str) -> Vec<LeftoverRecord> {
    let txt = |name: String| LeftoverRecord {
        name,
        record_type: "TXT".to_string(),
        // The rrset at these names is not ours to reconstruct, and the person
        // is deleting a Fauna entry from a name they own — never the whole set.
        value: String::new(),
        reason: LeftoverReason::SharedName,
    };
    vec![
        // SPF shares the apex with every other TXT the person publishes.
        txt(domain.to_string()),
        txt(format!("_dmarc.{domain}")),
        txt(format!("_mta-sts.{domain}")),
        txt(format!("_smtp._tls.{domain}")),
    ]
}

/// Build the cleanup plan for the **attributed** `domain` on a box at `ipv4`
/// (and `ipv6`) — the domain whose `mail.` is the deployment's MX host.
///
/// The address name set mirrors `fauna_mail::dns::host::build_host_dns_records`
/// — apex, `mail.`, `relay.`, `pds.` — plus the wildcard `*.`, which the host
/// builder does not emit but a person may have pointed at the box during
/// setup. `relay.`/`pds.` are planned unconditionally: they are emitted only
/// when those sidecars are enabled, and a value-scoped removal of a record
/// that does not exist is a no-op, whereas *missing* one strands an `A` on a
/// released address.
pub fn plan_dns_cleanup(domain: &str, ipv4: Option<&str>, ipv6: Option<&str>) -> RetireDnsPlan {
    plan_domain(domain, &mail_host(domain), true, ipv4, ipv6)
}

/// Build the cleanup plan for one **other** domain the box served — a
/// secondary local domain (`mail-multidomain.md` § Client reachability of a
/// secondary domain), whose apex `A`/`AAAA` the nest published → the box and
/// whose `MX` and DAV `SRV` target the **attributed** domain's mail host (one
/// MX host per deployment; there is no `mail.<secondary>`).
///
/// The same address name set as [`plan_dns_cleanup`], for the same reason
/// `relay.`/`pds.` are planned there: a secondary publishes only its apex, but
/// a half-done primary rename leaves a `mail.<new>` `A`, and a value-scoped
/// removal of a record that does not exist costs nothing. No `TLSA`: the floor
/// pin lives under the attributed mail host, which its own plan covers, and a
/// name-keyed removal under `mail.<secondary>` would be the one name sweep.
pub fn plan_secondary_dns_cleanup(
    domain: &str,
    attributed: &str,
    ipv4: Option<&str>,
    ipv6: Option<&str>,
) -> RetireDnsPlan {
    plan_domain(domain, &mail_host(attributed), false, ipv4, ipv6)
}

/// The per-domain plan both public builders share: the address name set of
/// `domain` scoped to the box's addresses, its `MX` and DAV `SRV` scoped to
/// `mail` (the deployment's MX host), the floor `TLSA` under `mail` only when
/// this is the domain that owns it, and the by-hand `TXT` list.
fn plan_domain(
    domain: &str,
    mail: &str,
    owns_mail_host: bool,
    ipv4: Option<&str>,
    ipv6: Option<&str>,
) -> RetireDnsPlan {
    let mut removals = Vec::new();

    // `A`/`AAAA` whose value is this box's address.
    let hosts = [
        domain.to_string(),
        format!("mail.{domain}"),
        format!("relay.{domain}"),
        format!("pds.{domain}"),
        format!("*.{domain}"),
    ];
    for host in &hosts {
        if let Some(v4) = ipv4 {
            removals.push(PlannedRemoval {
                name: host.clone(),
                record_type: "A".to_string(),
                value: v4.to_string(),
            });
        }
        if let Some(v6) = ipv6 {
            removals.push(PlannedRemoval {
                name: host.clone(),
                record_type: "AAAA".to_string(),
                value: v6.to_string(),
            });
        }
    }

    // The MX pointing at the deployment's mail host, and the RFC 6764 service
    // records that name the same host. Their value identifies the dying box's
    // role even though it is not an address.
    removals.push(PlannedRemoval {
        name: domain.to_string(),
        record_type: "MX".to_string(),
        value: mail.to_string(),
    });
    for service in ["_caldavs._tcp", "_carddavs._tcp"] {
        removals.push(PlannedRemoval {
            name: format!("{service}.{domain}"),
            record_type: "SRV".to_string(),
            value: mail.to_string(),
        });
    }

    // The floor TLSA under the mail host — keyed to the host, not to a value
    // we can reconstruct, so the whole rrset at that name goes. Only the
    // domain that owns the mail host plans it.
    if owns_mail_host {
        removals.push(PlannedRemoval {
            name: format!("_25._tcp.{mail}"),
            record_type: "TLSA".to_string(),
            value: String::new(),
        });
    }

    RetireDnsPlan {
        removals,
        leftovers: leftovers_for(domain),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_is_value_scoped_for_every_address_record() {
        let plan = plan_dns_cleanup("example.test", Some("203.0.113.5"), None);
        let addr: Vec<_> = plan
            .removals
            .iter()
            .filter(|r| r.record_type == "A" || r.record_type == "AAAA")
            .collect();
        assert!(!addr.is_empty());
        assert!(
            addr.iter().all(|r| r.value == "203.0.113.5"),
            "an address removal that is not value-scoped is a name sweep: {addr:?}"
        );
    }

    #[test]
    fn plan_covers_the_host_builders_whole_name_set_plus_the_wildcard() {
        let plan = plan_dns_cleanup("example.test", Some("203.0.113.5"), None);
        let a_names: Vec<&str> = plan
            .removals
            .iter()
            .filter(|r| r.record_type == "A")
            .map(|r| r.name.as_str())
            .collect();
        for expected in [
            "example.test",
            "mail.example.test",
            "relay.example.test",
            "pds.example.test",
            "*.example.test",
        ] {
            assert!(
                a_names.contains(&expected),
                "missing {expected} in {a_names:?}"
            );
        }
    }

    #[test]
    fn ipv6_records_are_planned_only_when_the_box_has_one() {
        let v4_only = plan_dns_cleanup("example.test", Some("203.0.113.5"), None);
        assert!(v4_only.removals.iter().all(|r| r.record_type != "AAAA"));

        let dual = plan_dns_cleanup("example.test", Some("203.0.113.5"), Some("2001:db8::5"));
        assert!(
            dual.removals
                .iter()
                .any(|r| r.record_type == "AAAA" && r.value == "2001:db8::5")
        );
    }

    #[test]
    fn mx_and_dav_srv_are_scoped_to_the_mail_host() {
        let plan = plan_dns_cleanup("example.test", Some("203.0.113.5"), None);
        let mx = plan
            .removals
            .iter()
            .find(|r| r.record_type == "MX")
            .expect("MX planned");
        assert_eq!(mx.value, "mail.example.test");
        let srv: Vec<_> = plan
            .removals
            .iter()
            .filter(|r| r.record_type == "SRV")
            .collect();
        assert_eq!(srv.len(), 2, "both _caldavs and _carddavs: {srv:?}");
        assert!(srv.iter().all(|r| r.value == "mail.example.test"));
    }

    #[test]
    fn spf_dmarc_mta_sts_and_tlsrpt_are_never_removed_only_listed() {
        let plan = plan_dns_cleanup("example.test", Some("203.0.113.5"), None);
        assert!(
            plan.removals.iter().all(|r| r.record_type != "TXT"),
            "no TXT is ever auto-removed: {:?}",
            plan.removals
        );
        let left: Vec<&str> = plan.leftovers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            left,
            vec![
                "example.test",
                "_dmarc.example.test",
                "_mta-sts.example.test",
                "_smtp._tls.example.test",
            ]
        );
    }

    #[test]
    fn the_by_hand_list_stands_alone_when_no_credential_is_usable() {
        let only = RetireDnsPlan::leftovers_only("example.test", Some("203.0.113.5"), None);
        assert!(
            only.removals.is_empty(),
            "no credential holds the zone — nothing is removed for the person"
        );
        assert!(
            only.leftovers.ends_with(&leftovers_for("example.test")),
            "the harmless-stale TXT are still listed, last: {:?}",
            only.leftovers
        );
    }

    #[test]
    fn the_by_hand_list_names_the_records_still_pointing_at_the_box() {
        let only = RetireDnsPlan::leftovers_only("example.test", Some("203.0.113.5"), None);
        let apex = only
            .leftovers
            .iter()
            .find(|l| l.name == "example.test" && l.record_type == "A")
            .expect("the apex A is what the takeover is made of");
        assert_eq!(
            apex.value, "203.0.113.5",
            "without the value the person cannot tell which entry to delete"
        );
        assert_eq!(apex.reason, LeftoverReason::PointsAtBox);

        for expected in ["MX", "SRV", "TLSA"] {
            assert!(
                only.leftovers.iter().any(|l| l.record_type == expected),
                "{expected} points at the dying box too: {:?}",
                only.leftovers
            );
        }
    }

    #[test]
    fn the_pointing_records_come_before_the_harmless_txt() {
        let only = RetireDnsPlan::leftovers_only("example.test", Some("203.0.113.5"), None);
        let first_txt = only
            .leftovers
            .iter()
            .position(|l| l.record_type == "TXT")
            .expect("the TXT are listed");
        assert!(
            only.leftovers[..first_txt]
                .iter()
                .all(|l| l.reason == LeftoverReason::PointsAtBox),
            "the urgent records lead the list: {:?}",
            only.leftovers
        );
        assert!(
            only.leftovers[first_txt..]
                .iter()
                .all(|l| l.reason == LeftoverReason::SharedName),
            "and nothing urgent hides below them: {:?}",
            only.leftovers
        );
    }

    #[test]
    fn the_by_hand_list_is_derived_from_the_plan_not_a_second_hand_written_set() {
        // The point of the shared `(domain, ipv4, ipv6)` signature: whatever
        // `plan_dns_cleanup` learns to remove, this list names, with no second
        // edit here — the `AAAA` included, now that the listing carries the
        // box's v6.
        let ipv4 = Some("203.0.113.5");
        let ipv6 = Some("2001:db8::5");
        let planned = plan_dns_cleanup("example.test", ipv4, ipv6).removals;
        let only = RetireDnsPlan::leftovers_only("example.test", ipv4, ipv6);
        let pointing: Vec<_> = only
            .leftovers
            .iter()
            .filter(|l| l.reason == LeftoverReason::PointsAtBox)
            .map(|l| (l.name.as_str(), l.record_type.as_str(), l.value.as_str()))
            .collect();
        let expected: Vec<_> = planned
            .iter()
            .map(|r| (r.name.as_str(), r.record_type.as_str(), r.value.as_str()))
            .collect();
        assert_eq!(pointing, expected);
    }

    #[test]
    fn an_address_less_row_still_lists_the_txt_and_the_value_less_records() {
        let only = RetireDnsPlan::leftovers_only("example.test", None, None);
        assert!(only.removals.is_empty());
        assert!(
            only.leftovers.iter().all(|l| l.record_type != "A"),
            "no address to scope an A to: {:?}",
            only.leftovers
        );
        assert!(
            only.leftovers
                .iter()
                .any(|l| l.record_type == "MX" && l.value == "mail.example.test"),
            "the MX names the dying box's role with no address needed"
        );
    }

    #[test]
    fn ptr_is_never_planned_it_dies_with_the_address() {
        let plan = plan_dns_cleanup("example.test", Some("203.0.113.5"), Some("2001:db8::5"));
        assert!(plan.removals.iter().all(|r| r.record_type != "PTR"));
    }

    // -- a box serving several domains (§ DNS cleanup → several domains) ----

    #[test]
    fn a_secondary_domain_plans_its_own_apex_a_value_scoped() {
        let plan = plan_secondary_dns_cleanup(
            "two.test",
            "example.test",
            Some("203.0.113.5"),
            Some("2001:db8::5"),
        );
        let apex_a = plan
            .removals
            .iter()
            .find(|r| r.name == "two.test" && r.record_type == "A")
            .expect("the second domain's apex A is what the takeover is made of");
        assert_eq!(apex_a.value, "203.0.113.5");
        let apex_aaaa = plan
            .removals
            .iter()
            .find(|r| r.name == "two.test" && r.record_type == "AAAA")
            .expect("and its AAAA when the box has a v6");
        assert_eq!(apex_aaaa.value, "2001:db8::5");
        assert!(
            plan.removals
                .iter()
                .filter(|r| r.record_type == "A" || r.record_type == "AAAA")
                .all(|r| r.name.ends_with("two.test")),
            "a secondary plan never reaches into another domain's names: {:?}",
            plan.removals
        );
    }

    #[test]
    fn a_secondary_domains_mx_and_srv_target_the_attributed_domains_mail_host() {
        // Every local domain's MX and DAV SRVs point at the deployment's one
        // MX host, `mail.<primary>` — there is no `mail.<secondary>`.
        let plan =
            plan_secondary_dns_cleanup("two.test", "example.test", Some("203.0.113.5"), None);
        let mx = plan
            .removals
            .iter()
            .find(|r| r.name == "two.test" && r.record_type == "MX")
            .expect("MX planned");
        assert_eq!(mx.value, "mail.example.test");
        let srv: Vec<_> = plan
            .removals
            .iter()
            .filter(|r| r.record_type == "SRV")
            .collect();
        assert_eq!(srv.len(), 2);
        assert!(srv.iter().all(|r| r.name.ends_with(".two.test")));
        assert!(srv.iter().all(|r| r.value == "mail.example.test"));
    }

    #[test]
    fn a_secondary_domain_never_plans_a_tlsa() {
        // The floor TLSA pins the MX host's leaf under `_25._tcp.mail.<primary>`,
        // which the attributed domain's plan already covers; a name-keyed
        // removal under `mail.<secondary>` would be the one name sweep here.
        let plan =
            plan_secondary_dns_cleanup("two.test", "example.test", Some("203.0.113.5"), None);
        assert!(plan.removals.iter().all(|r| r.record_type != "TLSA"));
    }

    #[test]
    fn a_secondary_domain_lists_its_own_txt_by_hand() {
        let plan =
            plan_secondary_dns_cleanup("two.test", "example.test", Some("203.0.113.5"), None);
        let left: Vec<&str> = plan.leftovers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            left,
            vec![
                "two.test",
                "_dmarc.two.test",
                "_mta-sts.two.test",
                "_smtp._tls.two.test",
            ]
        );
    }

    #[test]
    fn a_secondary_domains_by_hand_list_is_derived_from_its_plan() {
        let ipv4 = Some("203.0.113.5");
        let planned = plan_secondary_dns_cleanup("two.test", "example.test", ipv4, None);
        let by_hand = planned.clone().by_hand_only();
        assert!(by_hand.removals.is_empty());
        let pointing: Vec<_> = by_hand
            .leftovers
            .iter()
            .filter(|l| l.reason == LeftoverReason::PointsAtBox)
            .map(|l| (l.name.clone(), l.record_type.clone(), l.value.clone()))
            .collect();
        let expected: Vec<_> = planned
            .removals
            .iter()
            .map(|r| (r.name.clone(), r.record_type.clone(), r.value.clone()))
            .collect();
        assert_eq!(pointing, expected);
        assert!(by_hand.leftovers.ends_with(&leftovers_for("two.test")));
    }

    #[test]
    fn appending_a_secondary_plan_keeps_every_domains_removals_and_leftovers() {
        let mut plan = plan_dns_cleanup("example.test", Some("203.0.113.5"), None);
        let first_len = plan.removals.len();
        let first_left = plan.leftovers.len();
        plan.append(plan_secondary_dns_cleanup(
            "two.test",
            "example.test",
            Some("203.0.113.5"),
            None,
        ));
        assert!(plan.removals.len() > first_len);
        assert!(plan.leftovers.len() > first_left);
        assert!(
            plan.removals
                .iter()
                .any(|r| r.name == "example.test" && r.record_type == "A"),
            "the attributed domain's apex is still planned"
        );
        assert!(
            plan.removals
                .iter()
                .any(|r| r.name == "two.test" && r.record_type == "A"),
            "and so is the second domain's"
        );
    }
}
