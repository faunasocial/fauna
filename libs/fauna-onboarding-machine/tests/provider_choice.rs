//! The **choose-providers** page's model —
//! `docs/goal/behavior/onboarding-provisioning.md` § 3d (ratified 2026-11-15,
//! second and third rounds 2026-11-16: a bought domain's DNS may run at
//! another connected provider).
//!
//! One connect per provider runs every check its capabilities allow; the
//! machine decides which connected provider can cover each goal (registering
//! the domain while one is bought, DNS for it, a server), says why one cannot,
//! assigns each open goal preferring the fewest providers, and on Continue
//! writes the covers and the defaults into the `dns` / `vps` state the
//! unchanged orchestrator already reads, landing on the setting-things-up
//! page.

use std::collections::HashMap;
use std::sync::Arc;

use fauna_core::secret::SecretString;
use fauna_onboarding_machine::snapshots::HandleCheckOutcome;
use fauna_onboarding_machine::{
    GoalCover, HeldProviderCredential, OnboardingMachine, OnboardingStep, ProviderConnection,
    ProviderGoal, ProviderStatus, observer::CountingObserver,
};
use fauna_provisioning::dns::DnsZone;
use fauna_provisioning::progress::OverallStatus;
use fauna_provisioning::registrar::{ContactInfo, RegistrarAvailability};
use fauna_provisioning::vps::{ServerTypeInfo, VpsLocation};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── Fixtures ────────────────────────────────────────────────────────────────

/// A handle whose domain is to be bought (a reserved `.test` name).
const BUY_HANDLE: &str = "alice@fresh.test";

fn machine_for(handle: &str) -> Arc<OnboardingMachine> {
    let m = OnboardingMachine::new(CountingObserver::new());
    m.begin_create_identity();
    m.set_current_handle(handle.into());
    m
}

fn zone(name: &str) -> DnsZone {
    DnsZone {
        id: format!("zone-{}", name.replace(".", "-")),
        name: name.into(),
        nameservers: Vec::new(),
    }
}

/// A zone the provider assigned `nameservers` (a provider with the zone door).
fn zone_ns(name: &str, nameservers: &[&str]) -> DnsZone {
    DnsZone {
        nameservers: nameservers.iter().map(|n| n.to_string()).collect(),
        ..zone(name)
    }
}

fn location() -> VpsLocation {
    VpsLocation {
        id: "fsn1".into(),
        name: "Falkenstein".into(),
        city: "Falkenstein".into(),
        country: "DE".into(),
    }
}

fn offer(id: &str) -> ServerTypeInfo {
    priced(id, 4.0, 500)
}

fn priced(id: &str, mem_gb: f32, cents: u64) -> ServerTypeInfo {
    ServerTypeInfo {
        id: id.into(),
        vcpu: 2,
        mem_gb,
        disk_gb: 40,
        price_monthly_cents: cents,
        currency: "EUR".into(),
    }
}

/// A connected provider whose checks found `zones` (DNS) and, when
/// `server`, one location with one offer.
fn connected(zones: &[&str], server: bool) -> ProviderConnection {
    let mut creds = HashMap::new();
    creds.insert("api-token".to_string(), SecretString::from("t".to_string()));
    ProviderConnection {
        creds,
        connected: true,
        zones: zones.iter().map(|z| zone(z)).collect(),
        server_checked: server,
        locations: if server { vec![location()] } else { vec![] },
        server_types: if server { vec![offer("cx23")] } else { vec![] },
        listed_for_location: server.then(|| "fsn1".to_string()),
        ..Default::default()
    }
}

fn buying(m: &OnboardingMachine) {
    m.set_handle_check_outcome_for_test(HandleCheckOutcome::DomainAvailable {
        buyable_via_provider: true,
        price: None,
    });
    m.toggle_buy_domain(true);
    assert!(m.dns_config().buy_domain);
}

fn buyable() -> Option<RegistrarAvailability> {
    Some(RegistrarAvailability::Buyable {
        price_cents: 1200,
        currency: Some("EUR".into()),
        renewal_cents: Some(1500),
    })
}

/// A connected registrar quoting the domain as buyable.
fn registrar(server: bool) -> ProviderConnection {
    let mut c = connected(&[], server);
    c.availability = buyable();
    c
}

fn reason(m: &OnboardingMachine, id: &str, goal: ProviderGoal) -> Option<String> {
    let row = m
        .provider_rows()
        .into_iter()
        .find(|r| r.provider_id == id)
        .expect("a row per registry provider");
    match goal {
        ProviderGoal::Domain => row.domain_reason,
        ProviderGoal::Dns => row.dns_reason,
        ProviderGoal::Vps => row.vps_reason,
    }
    .map(|t| t.key)
}

fn goals(m: &OnboardingMachine) -> Vec<ProviderGoal> {
    m.goal_rows().into_iter().map(|r| r.goal).collect()
}

fn cover(m: &OnboardingMachine, goal: ProviderGoal) -> Option<GoalCover> {
    m.goal_rows()
        .into_iter()
        .find(|r| r.goal == goal)
        .expect("one row per goal")
        .cover
}

fn provider(id: &str) -> Option<GoalCover> {
    Some(GoalCover::Provider {
        provider_id: id.into(),
    })
}

fn blocked(m: &OnboardingMachine) -> Option<String> {
    m.providers_continue_blocked_reason().map(|t| t.key)
}

// ── The goal set and the can-cover rule ─────────────────────────────────────

#[test]
fn a_provider_holding_the_covering_zone_covers_both_goals() {
    let m = machine_for("alice@box.example.com");
    m.set_provider_connection_for_test("hetzner".into(), connected(&["example.com"], true));
    assert_eq!(reason(&m, "hetzner", ProviderGoal::Dns), None);
    assert_eq!(reason(&m, "hetzner", ProviderGoal::Vps), None);
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("hetzner"));
    assert_eq!(cover(&m, ProviderGoal::Vps), provider("hetzner"));
    assert!(m.can_continue_providers());
    assert!(blocked(&m).is_none());
}

#[test]
fn a_provider_without_the_zone_says_so_and_still_covers_the_server() {
    let m = machine_for("alice@example.com");
    m.set_provider_connection_for_test("hetzner".into(), connected(&["other.example"], true));
    assert_eq!(
        reason(&m, "hetzner", ProviderGoal::Dns).as_deref(),
        Some("onboarding.provider_choice.cannot_cover_zone_missing")
    );
    assert_eq!(cover(&m, ProviderGoal::Dns), None);
    assert_eq!(cover(&m, ProviderGoal::Vps), provider("hetzner"));
    assert!(!m.can_continue_providers());
    assert_eq!(
        blocked(&m).as_deref(),
        Some("onboarding.provider_choice.status_cover_dns")
    );
}

#[test]
fn the_registration_goal_is_on_the_page_only_while_buying() {
    let m = machine_for(BUY_HANDLE);
    assert_eq!(goals(&m), vec![ProviderGoal::Dns, ProviderGoal::Vps]);
    buying(&m);
    assert_eq!(
        goals(&m),
        vec![ProviderGoal::Domain, ProviderGoal::Dns, ProviderGoal::Vps]
    );
    assert_eq!(
        blocked(&m).as_deref(),
        Some("onboarding.provider_choice.status_cover_domain"),
        "the status line names the first open goal in page order"
    );
    m.toggle_buy_domain(false);
    assert_eq!(goals(&m), vec![ProviderGoal::Dns, ProviderGoal::Vps]);
}

#[test]
fn the_buy_box_is_offered_only_for_an_unregistered_domain() {
    let m = machine_for(BUY_HANDLE);
    assert!(!m.can_offer_buy_domain());
    m.toggle_buy_domain(true);
    assert!(
        !m.dns_config().buy_domain,
        "a domain the handle check did not find unregistered cannot be bought"
    );
    m.set_handle_check_outcome_for_test(HandleCheckOutcome::RegisteredNoNest);
    assert!(!m.can_offer_buy_domain());
    m.set_handle_check_outcome_for_test(HandleCheckOutcome::DomainAvailable {
        buyable_via_provider: false,
        price: None,
    });
    assert!(
        m.can_offer_buy_domain(),
        "either reading of DomainAvailable"
    );
    m.toggle_buy_domain(true);
    assert!(m.dns_config().buy_domain);
}

#[test]
fn a_registrar_covers_registration_and_dns_together() {
    let m = machine_for(BUY_HANDLE);
    buying(&m);
    m.set_provider_connection_for_test("porkbun".into(), registrar(false));
    assert_eq!(reason(&m, "porkbun", ProviderGoal::Domain), None);
    assert_eq!(reason(&m, "porkbun", ProviderGoal::Dns), None);
    assert_eq!(cover(&m, ProviderGoal::Domain), provider("porkbun"));
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("porkbun"));
    let domain = m
        .goal_rows()
        .into_iter()
        .find(|r| r.goal == ProviderGoal::Domain)
        .unwrap();
    assert_eq!(domain.price_cents, Some(1200), "the row states the price");
    assert_eq!(domain.renewal_cents, Some(1500));
    assert_eq!(domain.currency.as_deref(), Some("EUR"));

    // A registrar that cannot register this ending says so; nor can it run
    // DNS for a domain registered elsewhere — it creates no zones.
    let mut gandi = connected(&[], false);
    gandi.availability = Some(RegistrarAvailability::TldNotSupported);
    m.set_provider_connection_for_test("gandi".into(), gandi);
    assert_eq!(
        reason(&m, "gandi", ProviderGoal::Domain).as_deref(),
        Some("onboarding.provider_choice.cannot_cover_not_registrable")
    );
    assert_eq!(
        reason(&m, "gandi", ProviderGoal::Dns).as_deref(),
        Some("onboarding.provider_choice.cannot_cover_no_zone_door")
    );
}

#[test]
fn a_dns_host_with_the_zone_door_covers_dns_for_a_domain_bought_elsewhere() {
    let m = machine_for(BUY_HANDLE);
    buying(&m);
    // Hetzner runs DNS and rents servers but registers nothing.
    m.set_provider_connection_for_test("hetzner".into(), connected(&[], true));
    assert_eq!(
        reason(&m, "hetzner", ProviderGoal::Domain).as_deref(),
        Some("onboarding.provider_choice.cannot_cover_no_registrar")
    );
    assert_eq!(cover(&m, ProviderGoal::Domain), None);
    assert_eq!(
        blocked(&m).as_deref(),
        Some("onboarding.provider_choice.status_cover_domain")
    );
    // A DNS host that rents nothing says so, even unconnected.
    assert_eq!(
        reason(&m, "cloudflare", ProviderGoal::Vps).as_deref(),
        Some("onboarding.provider_choice.cannot_cover_no_server")
    );
    // Porkbun registers and can point the domain elsewhere: the DNS row starts
    // on the registrar and offers Hetzner.
    m.set_provider_connection_for_test("porkbun".into(), registrar(false));
    assert_eq!(cover(&m, ProviderGoal::Domain), provider("porkbun"));
    assert_eq!(reason(&m, "hetzner", ProviderGoal::Dns), None);
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("porkbun"));
    assert_eq!(cover(&m, ProviderGoal::Vps), provider("hetzner"));
    let dns = m
        .goal_rows()
        .into_iter()
        .find(|r| r.goal == ProviderGoal::Dns)
        .unwrap();
    assert_eq!(dns.candidates, vec!["porkbun", "hetzner"]);
    m.select_goal_cover(ProviderGoal::Dns, provider("hetzner").unwrap())
        .unwrap();
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("hetzner"));
    assert!(m.can_continue_providers());
}

#[test]
fn each_missing_delegation_door_is_named_on_the_dns_providers_row() {
    let m = machine_for(BUY_HANDLE);
    buying(&m);
    // Cloudflare registers it but cannot point a domain at another
    // provider's DNS; Hetzner could run the zone.
    let mut cloudflare = registrar(false);
    cloudflare.zones = vec![];
    m.set_provider_connection_for_test("cloudflare".into(), cloudflare);
    m.set_provider_connection_for_test("hetzner".into(), connected(&[], true));
    assert_eq!(cover(&m, ProviderGoal::Domain), provider("cloudflare"));
    let row = m
        .provider_rows()
        .into_iter()
        .find(|r| r.provider_id == "hetzner")
        .unwrap();
    let why = row.dns_reason.expect("the nameserver door is missing");
    assert_eq!(
        why.key,
        "onboarding.provider_choice.cannot_cover_no_nameserver_door"
    );
    assert_eq!(
        why.args.get("provider").map(String::as_str),
        Some("provisioning.cloudflare.name"),
        "the reason names the registration goal's cover by its display-name key"
    );
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("cloudflare"));
    // Porkbun with the nameserver door but no zone door: not a DNS cover for
    // a domain registered at Cloudflare.
    m.set_provider_connection_for_test("porkbun".into(), connected(&[], false));
    assert_eq!(
        reason(&m, "porkbun", ProviderGoal::Dns).as_deref(),
        Some("onboarding.provider_choice.cannot_cover_no_zone_door")
    );
}

#[test]
fn a_server_only_provider_covers_the_server_and_says_it_runs_no_dns() {
    let m = machine_for("alice@example.com");
    m.set_provider_connection_for_test("digitalocean".into(), connected(&[], true));
    assert_eq!(
        reason(&m, "digitalocean", ProviderGoal::Dns).as_deref(),
        Some("onboarding.provider_choice.cannot_cover_no_dns")
    );
    assert_eq!(reason(&m, "digitalocean", ProviderGoal::Vps), None);
    assert_eq!(
        reason(&m, "porkbun", ProviderGoal::Vps).as_deref(),
        Some("onboarding.provider_choice.cannot_cover_no_server")
    );
    assert_eq!(cover(&m, ProviderGoal::Vps), provider("digitalocean"));
    assert_eq!(
        blocked(&m).as_deref(),
        Some("onboarding.provider_choice.status_cover_dns")
    );
}

// ── Assignment ──────────────────────────────────────────────────────────────

#[test]
fn the_assignment_prefers_one_provider_covering_both() {
    let m = machine_for("alice@example.com");
    // porkbun comes first in the registry and can cover DNS; the bundled
    // provider can cover both, so it takes both goals.
    m.set_provider_connection_for_test("porkbun".into(), connected(&["example.com"], false));
    m.set_provider_connection_for_test("bundled".into(), connected(&["example.com"], true));
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("bundled"));
    assert_eq!(cover(&m, ProviderGoal::Vps), provider("bundled"));
    let dns = m
        .goal_rows()
        .into_iter()
        .find(|r| r.goal == ProviderGoal::Dns)
        .unwrap();
    assert_eq!(dns.candidates, vec!["porkbun", "bundled"]);

    // The person's choice on the goal row wins.
    m.select_goal_cover(ProviderGoal::Dns, provider("porkbun").unwrap())
        .unwrap();
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("porkbun"));
    assert_eq!(cover(&m, ProviderGoal::Vps), provider("bundled"));
    // A provider that cannot cover the goal is refused.
    assert!(
        m.select_goal_cover(ProviderGoal::Vps, provider("porkbun").unwrap())
            .is_err()
    );
}

#[test]
fn one_provider_with_every_capability_covers_all_three() {
    let m = machine_for(BUY_HANDLE);
    buying(&m);
    m.set_provider_connection_for_test("porkbun".into(), registrar(false));
    m.set_provider_connection_for_test("digitalocean".into(), connected(&[], true));
    // Bundled registers, runs DNS and rents servers: fewest providers wins.
    m.set_provider_connection_for_test("bundled".into(), registrar(true));
    assert_eq!(cover(&m, ProviderGoal::Domain), provider("bundled"));
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("bundled"));
    assert_eq!(cover(&m, ProviderGoal::Vps), provider("bundled"));
    // The person may still choose the other registrar.
    m.select_goal_cover(ProviderGoal::Domain, provider("porkbun").unwrap())
        .unwrap();
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("porkbun"));
    // porkbun rents no servers: the server goes to the first candidate.
    assert_eq!(cover(&m, ProviderGoal::Vps), provider("digitalocean"));
}

#[test]
fn the_manual_row_covers_dns_only() {
    let m = machine_for("alice@example.com");
    m.select_goal_cover(ProviderGoal::Dns, GoalCover::Manual)
        .unwrap();
    assert_eq!(cover(&m, ProviderGoal::Dns), Some(GoalCover::Manual));
    assert!(
        m.select_goal_cover(ProviderGoal::Vps, GoalCover::Manual)
            .is_err()
    );
    assert_eq!(
        blocked(&m).as_deref(),
        Some("onboarding.provider_choice.status_cover_vps")
    );
}

/// Rule 6 (`ui/README.md` § Offer only what is possible): each pick, the
/// providers Continue and the provisioning Start answer `Possibility` from
/// the same computation their actions ask first, so the page and the action
/// cannot disagree.
#[test]
fn the_provider_page_gates_answer_possibility_from_their_actions_rules() {
    let m = machine_for("alice@example.com");
    // The manual row: always possible for DNS, refused for the server with
    // the reason `select_goal_cover` gives.
    assert!(
        m.goal_cover_possibility(ProviderGoal::Dns, GoalCover::Manual)
            .is_possible()
    );
    let vps_manual = m.goal_cover_possibility(ProviderGoal::Vps, GoalCover::Manual);
    assert_eq!(
        vps_manual.reason().map(|t| t.key.as_str()),
        Some("onboarding.provider_choice.status_cover_vps")
    );
    assert!(
        m.select_goal_cover(ProviderGoal::Vps, GoalCover::Manual)
            .is_err()
    );
    // A provider not connected is blocked with a reason, and the pick is
    // refused alike.
    assert!(
        !m.goal_cover_possibility(ProviderGoal::Dns, provider("hetzner").unwrap())
            .is_possible()
    );
    // Continue: blocked while a goal is open, by the same key the reason
    // getter gives.
    assert_eq!(
        m.providers_continue_possibility()
            .reason()
            .map(|t| t.key.clone()),
        blocked(&m)
    );
    m.set_provider_connection_for_test("hetzner".into(), connected(&["example.com"], true));
    assert!(m.providers_continue_possibility().is_possible());
    // Start: the routing guard folded in — possible off the choose-providers
    // path, and asked of the machine once entered from it.
    assert!(m.provisioning_start_possibility().is_possible());
}

#[test]
fn the_manual_row_covers_dns_while_buying_too() {
    let m = machine_for(BUY_HANDLE);
    let manual = |m: &OnboardingMachine| {
        m.provider_rows()
            .into_iter()
            .find(|r| r.provider_id == "manual")
            .expect("the manual row is one of the rows")
    };
    m.select_goal_cover(ProviderGoal::Dns, GoalCover::Manual)
        .unwrap();
    buying(&m);
    assert!(manual(&m).dns_reason.is_none());
    assert_eq!(cover(&m, ProviderGoal::Dns), Some(GoalCover::Manual));
    m.set_provider_connection_for_test("porkbun".into(), registrar(false));
    m.set_provider_connection_for_test("digitalocean".into(), connected(&[], true));
    assert_eq!(cover(&m, ProviderGoal::Domain), provider("porkbun"));
    assert_eq!(cover(&m, ProviderGoal::Dns), Some(GoalCover::Manual));
    // Continue takes the deferred-DNS path that still registers: the price
    // must be agreed before it may start.
    m.continue_from_providers().unwrap();
    assert!(m.dns_config().set_up_later);
    assert!(m.dns_config().buy_domain);
    assert_eq!(
        m.provisioning_start_blocked_reason()
            .map(|t| t.key)
            .as_deref(),
        Some("onboarding.provider_choice.start_blocked_price")
    );
    // The button's answer carries the same reason once entered from the
    // choose-providers page (rule 6).
    assert_eq!(
        m.provisioning_start_possibility()
            .reason()
            .map(|t| t.key.as_str()),
        Some("onboarding.provider_choice.start_blocked_price")
    );
    // The run still registers, so the page carries what any buying run
    // carries (`onboarding-provisioning.md` § 6 *Deferred-DNS path*): the
    // domain line with the registrar's quote, and the price box under it.
    let domain_line = m
        .bill_of_materials()
        .into_iter()
        .find(|i| !i.recurring)
        .expect("the domain line is on the page");
    assert_eq!(domain_line.price_cents, 1200);
    assert_eq!(domain_line.renewal_price_cents, Some(1500));
    assert!(m.bom_domain_line().is_some());
    assert!(m.provisioning_price_confirm_visible());
    m.confirm_price();
    assert!(m.provisioning_start_blocked_reason().is_none());
    assert!(m.provisioning_start_possibility().is_possible());
    assert!(
        m.provisioning_price_confirm_visible(),
        "agreed, the box stays under the line, ticked"
    );
}

#[test]
fn no_price_box_when_nothing_is_bought() {
    let m = machine_for(BUY_HANDLE);
    m.select_goal_cover(ProviderGoal::Dns, GoalCover::Manual)
        .unwrap();
    m.set_provider_connection_for_test("digitalocean".into(), connected(&[], true));
    m.continue_from_providers().unwrap();
    assert!(!m.dns_config().buy_domain);
    assert!(m.bill_of_materials().iter().all(|i| i.recurring));
    assert!(!m.provisioning_price_confirm_visible());
}

#[test]
fn back_after_a_registration_keeps_the_registration_goal_fixed() {
    let m = machine_for(BUY_HANDLE);
    buying(&m);
    m.set_provider_connection_for_test("porkbun".into(), registrar(false));
    m.set_provider_connection_for_test("hetzner".into(), connected(&[], true));
    let mut gandi = registrar(false);
    gandi.availability = buyable();
    m.set_provider_connection_for_test("gandi".into(), gandi);
    m.set_wizard_memory_for_test(Some("porkbun".into()), None);

    assert!(
        !m.can_offer_buy_domain(),
        "a registered domain cannot be bought"
    );
    m.toggle_buy_domain(false);
    assert!(
        goals(&m).contains(&ProviderGoal::Domain),
        "the registration goal stays on the page"
    );
    assert_eq!(cover(&m, ProviderGoal::Domain), provider("porkbun"));
    assert!(
        m.select_goal_cover(ProviderGoal::Domain, provider("gandi").unwrap())
            .is_err(),
        "the registration goal is fixed"
    );
    assert_eq!(
        reason(&m, "gandi", ProviderGoal::Domain).as_deref(),
        Some("onboarding.provider_choice.cannot_cover_not_registrable")
    );
    // Not pointed anywhere yet: any cover § 3d allows.
    m.select_goal_cover(ProviderGoal::Dns, provider("hetzner").unwrap())
        .unwrap();
    m.select_goal_cover(ProviderGoal::Dns, GoalCover::Manual)
        .unwrap();
    m.select_goal_cover(ProviderGoal::Dns, provider("porkbun").unwrap())
        .unwrap();
    // A Retry needs no price agreed again: nothing more is bought.
    m.continue_from_providers().unwrap();
    assert!(m.provisioning_start_blocked_reason().is_none());
    assert!(!m.provisioning_price_confirm_visible(), "no price to agree");
}

#[test]
fn back_after_the_nameserver_set_leaves_that_provider_or_the_manual_row() {
    let m = machine_for(BUY_HANDLE);
    buying(&m);
    m.set_provider_connection_for_test("porkbun".into(), registrar(false));
    m.set_provider_connection_for_test("hetzner".into(), connected(&[], true));
    m.set_wizard_memory_for_test(Some("porkbun".into()), Some("hetzner".into()));
    let dns = m
        .goal_rows()
        .into_iter()
        .find(|r| r.goal == ProviderGoal::Dns)
        .unwrap();
    assert_eq!(dns.candidates, vec!["hetzner"]);
    assert_eq!(dns.cover, provider("hetzner"));
    let why = m
        .provider_rows()
        .into_iter()
        .find(|r| r.provider_id == "porkbun")
        .unwrap()
        .dns_reason
        .expect("the registrar's own zone no longer answers");
    assert_eq!(
        why.key,
        "onboarding.provider_choice.cannot_cover_pointed_elsewhere"
    );
    assert_eq!(
        why.args.get("provider").map(String::as_str),
        Some("provisioning.hetzner.name")
    );
    assert!(
        m.select_goal_cover(ProviderGoal::Dns, provider("porkbun").unwrap())
            .is_err()
    );
    m.select_goal_cover(ProviderGoal::Dns, GoalCover::Manual)
        .unwrap();
    assert_eq!(cover(&m, ProviderGoal::Dns), Some(GoalCover::Manual));
}

#[test]
fn the_memory_is_the_handles_domains_only() {
    let m = machine_for(BUY_HANDLE);
    m.set_handle_check_outcome_for_test(HandleCheckOutcome::DomainAvailable {
        buyable_via_provider: true,
        price: None,
    });
    m.set_wizard_memory_for_test(Some("porkbun".into()), None);
    assert!(!m.can_offer_buy_domain());
    m.set_current_handle("alice@other.test".into());
    m.set_handle_check_outcome_for_test(HandleCheckOutcome::DomainAvailable {
        buyable_via_provider: true,
        price: None,
    });
    assert!(
        m.can_offer_buy_domain(),
        "a registration of another domain does not count"
    );
}

// ── A zone the domain does not point at ────────────────────────────────────

#[test]
fn the_assignment_prefers_the_zone_the_delegation_names() {
    let m = machine_for("alice@example.com");
    m.set_provider_connection_for_test("digitalocean".into(), connected(&[], true));
    let mut cloudflare = connected(&[], false);
    cloudflare.zones = vec![zone_ns("example.com", &["ada.ns.other-host.example"])];
    m.set_provider_connection_for_test("cloudflare".into(), cloudflare);
    let mut hetzner = connected(&[], false);
    hetzner.zones = vec![zone_ns(
        "example.com",
        &[
            "hydrogen.ns.zone-host.example.",
            "oxygen.ns.zone-host.example.",
        ],
    )];
    m.set_provider_connection_for_test("hetzner".into(), hetzner);
    let dns_row = |m: &OnboardingMachine| {
        m.goal_rows()
            .into_iter()
            .find(|r| r.goal == ProviderGoal::Dns)
            .unwrap()
    };

    // The domain points at Hetzner: Hetzner, though Cloudflare comes first.
    m.set_handle_delegation_for_test(vec!["oxygen.ns.zone-host.example".into()]);
    assert_eq!(dns_row(&m).cover, provider("hetzner"));
    assert_eq!(dns_row(&m).note, None);
    assert_eq!(dns_row(&m).candidates, vec!["cloudflare", "hetzner"]);

    // Choosing the zone it does not point at is allowed, and says so.
    m.select_goal_cover(ProviderGoal::Dns, provider("cloudflare").unwrap())
        .unwrap();
    let note = dns_row(&m).note.expect("the not-pointed-yet note");
    assert_eq!(note.key, "onboarding.provider_choice.dns_not_pointed_yet");
    assert_eq!(
        note.args.get("domain").map(String::as_str),
        Some("example.com")
    );
    assert_eq!(
        note.args.get("provider").map(String::as_str),
        Some("provisioning.cloudflare.name")
    );

    // Pointed at neither: the first candidate, with the note.
    let m2 = machine_for("alice@example.com");
    let mut cf = connected(&[], false);
    cf.zones = vec![zone_ns("example.com", &["ada.ns.other-host.example"])];
    m2.set_provider_connection_for_test("cloudflare".into(), cf);
    m2.set_handle_delegation_for_test(vec!["ns1.elsewhere.example".into()]);
    assert_eq!(dns_row(&m2).cover, provider("cloudflare"));
    assert!(dns_row(&m2).note.is_some());
}

#[test]
fn continue_writes_a_split_cover_for_the_run() {
    let m = machine_for(BUY_HANDLE);
    buying(&m);
    m.set_provider_connection_for_test("porkbun".into(), registrar(false));
    m.set_provider_connection_for_test("hetzner".into(), connected(&[], true));
    m.select_goal_cover(ProviderGoal::Dns, provider("hetzner").unwrap())
        .unwrap();
    m.continue_from_providers().unwrap();
    let dns = m.dns_config();
    assert_eq!(dns.selected_provider_id.as_deref(), Some("hetzner"));
    assert!(!dns.set_up_later);
    assert!(
        matches!(
            dns.current_availability,
            Some(RegistrarAvailability::Buyable {
                price_cents: 1200,
                ..
            })
        ),
        "the price agreed is the registrar's"
    );
    assert_eq!(
        m.captured_dns_credential()
            .map(|c| c.provider_id)
            .as_deref(),
        Some("hetzner"),
        "the DNS credential kept is the DNS provider's"
    );
    assert_eq!(
        m.provisioning_start_blocked_reason()
            .map(|t| t.key)
            .as_deref(),
        Some("onboarding.provider_choice.start_blocked_price")
    );
}

#[test]
fn disconnect_uncovers_what_the_provider_covered() {
    let m = machine_for("alice@example.com");
    m.set_provider_connection_for_test("hetzner".into(), connected(&["example.com"], true));
    m.select_goal_cover(ProviderGoal::Vps, provider("hetzner").unwrap())
        .unwrap();
    m.disconnect_provider("hetzner".into());
    assert_eq!(cover(&m, ProviderGoal::Dns), None);
    assert_eq!(cover(&m, ProviderGoal::Vps), None);
    assert_eq!(
        blocked(&m).as_deref(),
        Some("onboarding.provider_choice.status_cover_dns")
    );
    assert!(!m.provider_rows().iter().any(|r| r.connected));
}

// ── The provider's dialog ───────────────────────────────────────────────────

#[test]
fn the_dialog_shows_registrar_fields_only_when_buying() {
    let m = machine_for(BUY_HANDLE);
    let ids = |m: &OnboardingMachine, p: &str| -> Vec<String> {
        m.visible_provider_fields(p.into())
            .into_iter()
            .map(|f| f.id)
            .collect()
    };
    assert_eq!(ids(&m, "cloudflare"), vec!["api-token"]);
    assert_eq!(ids(&m, "hetzner"), vec!["api-token"]);
    buying(&m);
    assert_eq!(ids(&m, "cloudflare"), vec!["api-token", "account-id"]);
    assert!(!m.can_connect_provider("cloudflare".into()));
    m.set_provider_cred("cloudflare".into(), "api-token".into(), "t".into());
    m.set_provider_cred("cloudflare".into(), "account-id".into(), "a".into());
    assert!(m.can_connect_provider("cloudflare".into()));
}

/// The connect dialog's Connect and sign-in buttons answer with a reason
/// (`ui/README.md` § Offer only what is possible, rules 2 and 6).
#[test]
fn the_connect_and_sign_in_answers_name_what_revives_them() {
    use fauna_core::possibility::Possibility;
    let key = |p: Possibility| p.reason().map(|r| r.key.clone());
    let m = machine_for(BUY_HANDLE);
    assert_eq!(
        key(m.connect_provider_possibility("cloudflare".into())).as_deref(),
        Some("provisioning.fill_required_fields")
    );
    m.set_provider_cred("cloudflare".into(), "api-token".into(), "t".into());
    assert!(
        m.connect_provider_possibility("cloudflare".into())
            .is_possible()
    );
    assert_eq!(
        m.connect_provider_possibility("no-such-provider".into()),
        Possibility::Absent,
        "an unknown provider has no dialog to paint a button in"
    );
    assert_eq!(
        key(m.provider_hosted_auth_possibility("bundled".into(), "api-token".into())).as_deref(),
        Some("provisioning.hosted_auth.base_url_first")
    );
    m.set_provider_cred(
        "bundled".into(),
        "base-url".into(),
        "https://provider.example".into(),
    );
    assert!(
        m.provider_hosted_auth_possibility("bundled".into(), "api-token".into())
            .is_possible()
    );
}

#[test]
fn the_dialog_shows_a_registrar_note_or_the_contact_form() {
    let m = machine_for(BUY_HANDLE);
    assert_eq!(m.provider_registrar_notes_key("porkbun".into()), None);
    buying(&m);
    m.set_provider_connection_for_test("porkbun".into(), registrar(false));
    assert_eq!(
        m.provider_registrar_notes_key("porkbun".into()).as_deref(),
        Some("provisioning.porkbun.registrar_account_contact_note")
    );
    assert!(!m.provider_shows_contact_form("porkbun".into()));
    assert_eq!(m.provider_registrar_notes_key("hetzner".into()), None);
}

#[test]
fn continue_waits_for_the_contact_a_registrar_requires() {
    let m = machine_for(BUY_HANDLE);
    buying(&m);
    m.set_provider_connection_for_test("bundled".into(), registrar(true));
    assert!(m.provider_shows_contact_form("bundled".into()));
    assert!(!m.can_continue_providers());
    let why = m.providers_continue_blocked_reason().unwrap();
    assert_eq!(why.key, "onboarding.provider_choice.status_contact_missing");
    // The registrar's display-name key, so every app's nested resolve names
    // it as its row does — never the registry id (onboarding-provisioning.md
    // § 3d, a reason's `{provider}`).
    assert_eq!(
        why.args.get("provider").map(String::as_str),
        Some("provisioning.bundled.name")
    );
    assert!(m.continue_from_providers().is_err());

    let contact = ContactInfo {
        first_name: "A".into(),
        country: "NO".into(),
        ..Default::default()
    };
    m.set_provider_contact("bundled".into(), contact.clone());
    assert_eq!(m.provider_contact("bundled".into()), Some(contact.clone()));
    assert!(m.can_continue_providers());
    m.continue_from_providers().unwrap();
    assert_eq!(m.dns_config().contact, Some(contact));
}

// ── Connect, against a fake Hetzner ────────────────────────────────────────

async fn mount_hetzner_checks(server: &MockServer, zones_status: u16) {
    Mock::given(method("GET"))
        .and(path("/zones"))
        .respond_with(
            ResponseTemplate::new(zones_status).set_body_json(serde_json::json!({
                "zones": [{"id": 1, "name": "example.com"}]
            })),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/locations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "locations": [{"name": "fsn1", "description": "Falkenstein DC Park 1",
                           "city": "Falkenstein", "country": "DE"}]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/server_types"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "server_types": [{"name": "cx23", "cores": 2, "memory": 4.0, "disk": 40,
                "prices": [{"location": "fsn1", "price_monthly": {"gross": "4.5100"}}]}]
        })))
        .mount(server)
        .await;
}

fn faked(server: &MockServer, handle: &str) -> Arc<OnboardingMachine> {
    let urls: HashMap<String, String> = ["vps", "dns", "nest"]
        .into_iter()
        .map(|k| (k.to_string(), server.uri()))
        .collect();
    let m = OnboardingMachine::new_with_provider_base_urls(CountingObserver::new(), Some(urls));
    m.begin_create_identity();
    m.set_current_handle(handle.into());
    m
}

fn row(m: &OnboardingMachine, id: &str) -> fauna_onboarding_machine::ProviderRow {
    m.provider_rows()
        .into_iter()
        .find(|r| r.provider_id == id)
        .unwrap()
}

#[tokio::test]
async fn one_connect_runs_every_check_the_provider_has() {
    let server = MockServer::start().await;
    mount_hetzner_checks(&server, 200).await;
    let m = faked(&server, "alice@example.com");
    m.set_provider_cred("hetzner".into(), "api-token".into(), "tok".into());
    m.connect_provider("hetzner".into()).await.unwrap();
    let r = row(&m, "hetzner");
    assert!(r.connected && !r.held && r.error.is_none());
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("hetzner"));
    assert_eq!(cover(&m, ProviderGoal::Vps), provider("hetzner"));
}

#[tokio::test]
async fn a_failed_check_leaves_the_provider_unconnected_with_its_error() {
    let server = MockServer::start().await;
    mount_hetzner_checks(&server, 401).await;
    let m = faked(&server, "alice@example.com");
    m.set_provider_cred("hetzner".into(), "api-token".into(), "bad".into());
    assert!(m.connect_provider("hetzner".into()).await.is_err());
    let r = row(&m, "hetzner");
    assert!(!r.connected);
    assert!(r.error.is_some());
    assert_eq!(cover(&m, ProviderGoal::Vps), None);
}

fn held(token: &str) -> HeldProviderCredential {
    HeldProviderCredential {
        provider_id: "hetzner".into(),
        fields: [(
            "api-token".to_string(),
            SecretString::from(token.to_string()),
        )]
        .into_iter()
        .collect(),
    }
}

#[tokio::test]
async fn a_held_credential_is_re_checked_before_it_reads_connected() {
    let server = MockServer::start().await;
    mount_hetzner_checks(&server, 200).await;
    let m = faked(&server, "alice@example.com");
    m.seed_held_provider_credentials(vec![held("good")]).await;
    let r = row(&m, "hetzner");
    assert!(r.connected && r.held);
    assert_eq!(cover(&m, ProviderGoal::Vps), provider("hetzner"));
}

/// The binding faces' form: the native apps read the account's held DNS
/// credentials as `retire_held_inputs`'s index-paired ids and JSON bags, and
/// hand those straight in — no app projects the credential bag itself.
#[tokio::test]
async fn the_json_form_seeds_the_same_held_credential() {
    let server = MockServer::start().await;
    mount_hetzner_checks(&server, 200).await;
    let m = faked(&server, "alice@example.com");
    m.seed_held_provider_credentials_json(
        vec!["hetzner".into(), "orphan-without-a-bag".into()],
        vec![r#"{"api-token":"good"}"#.into()],
    )
    .await;
    let r = row(&m, "hetzner");
    assert!(r.connected && r.held);
    assert_eq!(cover(&m, ProviderGoal::Dns), provider("hetzner"));
}

#[tokio::test]
async fn a_held_credential_that_fails_its_re_check_reads_not_connected() {
    let server = MockServer::start().await;
    mount_hetzner_checks(&server, 401).await;
    let m = faked(&server, "alice@example.com");
    m.seed_held_provider_credentials(vec![held("revoked")])
        .await;
    let r = row(&m, "hetzner");
    assert!(
        !r.connected && !r.held,
        "a revoked token reads as not connected"
    );
    assert!(
        r.error.is_none(),
        "a dropped held credential leaves no error"
    );
}

// ── Continue writes the covers and the defaults, then the build page ───────

#[test]
fn continue_writes_one_provider_into_both_states_and_lands_on_the_build_page() {
    let m = machine_for("alice@example.com");
    m.set_provider_connection_for_test("hetzner".into(), connected(&["example.com"], true));
    m.continue_from_providers().unwrap();
    assert_eq!(m.step(), OnboardingStep::NestProvisioning);
    let dns = m.dns_config();
    assert_eq!(dns.selected_provider_id.as_deref(), Some("hetzner"));
    assert!(dns.verified && !dns.set_up_later && !dns.same_provider_for_vps);
    assert_eq!(dns.zone_id.as_deref(), Some("zone-example-com"));
    assert_eq!(m.provider_status(), ProviderStatus::ProviderHasDomain);
    assert!(m.can_continue_dns());
    assert_eq!(
        m.captured_dns_credential()
            .map(|c| c.provider_id)
            .as_deref(),
        Some("hetzner")
    );
    let vps = m.vps_config();
    assert_eq!(vps.selected_provider_id.as_deref(), Some("hetzner"));
    assert!(vps.verified);
    assert_eq!(vps.selected_location_id.as_deref(), Some("fsn1"));
    assert_eq!(vps.server_types.len(), 1, "offers already listed");
    assert_eq!(vps.selected_server_type_id.as_deref(), Some("cx23"));
    assert!(m.provisioning_choices_visible());
    assert!(m.provisioning_back_to_providers());
    assert!(m.provisioning_start_blocked_reason().is_none());
}

#[test]
fn continue_while_buying_lands_on_the_build_page_with_the_price_unagreed() {
    let m = machine_for(BUY_HANDLE);
    buying(&m);
    m.set_provider_connection_for_test("porkbun".into(), registrar(false));
    m.set_provider_connection_for_test("digitalocean".into(), connected(&[], true));
    m.continue_from_providers().unwrap();
    assert_eq!(m.step(), OnboardingStep::NestProvisioning);
    assert!(matches!(
        m.provider_status(),
        ProviderStatus::UnregisteredBuyable {
            price_cents: 1200,
            ..
        }
    ));
    assert_eq!(
        m.vps_config().selected_provider_id.as_deref(),
        Some("digitalocean")
    );
    assert!(!m.dns_config().price_agreed, "Continue agrees to no price");
    assert_eq!(
        m.provisioning_start_blocked_reason()
            .map(|t| t.key)
            .as_deref(),
        Some("onboarding.provider_choice.start_blocked_price")
    );
    m.confirm_price();
    assert!(m.provisioning_start_blocked_reason().is_none());
}

#[test]
fn continue_chooses_the_cheapest_offer_the_mail_gate_permits() {
    let m = machine_for("alice@example.com");
    let mut hetzner = connected(&["example.com"], true);
    hetzner.server_types = vec![
        priced("big", 8.0, 900),
        priced("small", 1.0, 300),
        priced("mid", 4.0, 500),
    ];
    m.set_provider_connection_for_test("hetzner".into(), hetzner);
    m.set_provision_mail_mode(false);
    m.continue_from_providers().unwrap();
    let selected = |m: &OnboardingMachine| m.vps_config().selected_server_type_id;
    assert_eq!(selected(&m).as_deref(), Some("small"));
    assert_eq!(m.vps_config().selected_location_id.as_deref(), Some("fsn1"));
    // Mail on drops the 1 GB plan; the type is re-chosen, never left empty.
    m.set_provision_mail_mode(true);
    assert_eq!(selected(&m).as_deref(), Some("mid"));
    // Mail off again keeps a selection that is still allowed.
    m.set_provision_mail_mode(false);
    assert_eq!(selected(&m).as_deref(), Some("mid"));
}

#[test]
fn the_build_page_says_why_it_cannot_start() {
    let m = machine_for("alice@example.com");
    let mut hetzner = connected(&["example.com"], true);
    hetzner.server_types = vec![];
    m.set_provider_connection_for_test("hetzner".into(), hetzner);
    m.continue_from_providers().unwrap();
    assert_eq!(
        m.provisioning_start_blocked_reason()
            .map(|t| t.key)
            .as_deref(),
        Some("onboarding.provider_choice.start_blocked_server_type")
    );
}

#[test]
fn continue_is_refused_while_a_goal_is_open() {
    let m = machine_for("alice@example.com");
    assert!(m.continue_from_providers().is_err());
}

// ── Refusals are never silent (ui/README.md § Offer only what is possible,
// rule 7): each leaves its reason on the shared `error_message` ─────────────

use fauna_i18n::strings::onboarding::provider_choice as pc;

#[test]
fn a_refused_continue_leaves_its_reason_on_the_error_channel() {
    let m = machine_for("alice@example.com");
    let err = m.continue_from_providers().unwrap_err();
    assert_eq!(m.error_message().as_deref(), Some(pc::STATUS_COVER_DNS));
    assert!(err.to_string().contains(pc::STATUS_COVER_DNS), "{err}");
}

#[test]
fn a_refused_manual_pick_leaves_its_reason_and_an_accepted_pick_clears_it() {
    let m = machine_for("alice@example.com");
    assert!(
        m.select_goal_cover(ProviderGoal::Vps, GoalCover::Manual)
            .is_err()
    );
    assert_eq!(m.error_message().as_deref(), Some(pc::STATUS_COVER_VPS));
    m.select_goal_cover(ProviderGoal::Dns, GoalCover::Manual)
        .unwrap();
    assert_eq!(m.error_message(), None);
}

#[test]
fn a_pick_of_an_unconnected_provider_says_to_connect_it() {
    let m = machine_for("alice@example.com");
    assert!(
        m.select_goal_cover(ProviderGoal::Vps, provider("hetzner").unwrap())
            .is_err()
    );
    let msg = m.error_message().expect("the refusal names its reason");
    assert!(msg.starts_with("Connect ") && !msg.contains('{'), "{msg}");
    assert!(
        !msg.contains("provider."),
        "the display name is resolved: {msg}"
    );
}

#[test]
fn a_pick_the_can_cover_rule_refuses_names_that_rule() {
    let m = machine_for("alice@example.com");
    // Connected for the server only: it holds no zone for the domain.
    m.set_provider_connection_for_test("hetzner".into(), connected(&[], true));
    assert!(
        m.select_goal_cover(ProviderGoal::Dns, provider("hetzner").unwrap())
            .is_err()
    );
    assert_eq!(
        m.error_message().as_deref(),
        Some(pc::cannot_cover_zone_missing("example.com").as_str())
    );
}

#[test]
fn a_registration_pick_while_nothing_is_bought_says_so() {
    let m = machine_for("alice@example.com");
    m.set_provider_connection_for_test("bundled".into(), registrar(true));
    assert!(
        m.select_goal_cover(ProviderGoal::Domain, provider("bundled").unwrap())
            .is_err()
    );
    assert_eq!(m.error_message().as_deref(), Some(pc::REFUSED_NOT_BUYING));
}

#[tokio::test]
async fn a_refused_start_leaves_its_reason_on_the_error_channel() {
    let m = machine_for("alice@example.com");
    let mut hetzner = connected(&["example.com"], true);
    hetzner.server_types = vec![];
    m.set_provider_connection_for_test("hetzner".into(), hetzner);
    m.continue_from_providers().unwrap();
    m.clone().run_provisioning().await;
    assert_eq!(
        m.error_message().as_deref(),
        Some(pc::START_BLOCKED_SERVER_TYPE)
    );
    assert_eq!(m.provisioning_snapshot().overall, OverallStatus::Idle);
}

/// A location change whose offers drop the selected type re-chooses it.
#[tokio::test]
async fn a_new_location_re_chooses_a_dropped_server_type() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/zones"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "zones": [{"id": 1, "name": "example.com"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/locations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "locations": [
                {"name": "fsn1", "description": "Falkenstein DC Park 1",
                 "city": "Falkenstein", "country": "DE"},
                {"name": "nbg1", "description": "Nuremberg DC Park 1",
                 "city": "Nuremberg", "country": "DE"}]
        })))
        .mount(&server)
        .await;
    let types = |names: &[(&str, &str)]| {
        serde_json::json!({ "server_types": names.iter().map(|(n, gross)| serde_json::json!({
            "name": n, "cores": 2, "memory": 4.0, "disk": 40,
            "prices": [{"location": "fsn1", "price_monthly": {"gross": gross}}]
        })).collect::<Vec<_>>() })
    };
    // The connect lists cx23 (the cheapest) and cx33; the later listing for
    // the new location has no cx23.
    Mock::given(method("GET"))
        .and(path("/server_types"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(types(&[("cx23", "4.5100"), ("cx33", "7.0000")])),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/server_types"))
        .respond_with(ResponseTemplate::new(200).set_body_json(types(&[("cx33", "7.0000")])))
        .with_priority(2)
        .mount(&server)
        .await;
    let m = faked(&server, "alice@example.com");
    m.set_provider_cred("hetzner".into(), "api-token".into(), "tok".into());
    m.connect_provider("hetzner".into()).await.unwrap();
    m.continue_from_providers().unwrap();
    assert_eq!(
        m.vps_config().selected_server_type_id.as_deref(),
        Some("cx23")
    );
    m.select_vps_location("nbg1".into());
    for _ in 0..250 {
        if m.vps_config().selected_server_type_id.as_deref() != Some("cx23") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        m.vps_config().selected_server_type_id.as_deref(),
        Some("cx33"),
        "a type is always selected while offers exist"
    );
}

async fn mount_cloud(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/servers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"servers": []})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/servers"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_string(r#"{"server":{"id":42,"public_net":{"ipv4":{"ip":"5.6.7.8"}}}}"#),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/servers/42/actions/change_dns_ptr"))
        .respond_with(ResponseTemplate::new(201))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/servers/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "server": {"public_net": {"ipv4": {"dns_ptr": null}}}
        })))
        .mount(server)
        .await;
}

/// The manual DNS cover plus a connected Hetzner server, through Continue
/// straight to the build page, run by the unchanged orchestrator (the
/// deferred-DNS path) to `Succeeded`.
#[tokio::test]
async fn a_run_started_from_the_providers_page_provisions_unchanged() {
    let server = MockServer::start().await;
    mount_hetzner_checks(&server, 200).await;
    mount_cloud(&server).await;
    let m = faked(&server, "alice@example.com");
    m.set_provider_cred("hetzner".into(), "api-token".into(), "tok".into());
    m.connect_provider("hetzner".into()).await.unwrap();
    m.select_goal_cover(ProviderGoal::Dns, GoalCover::Manual)
        .unwrap();
    m.set_provision_mail_mode(false);
    m.continue_from_providers().unwrap();
    assert_eq!(m.step(), OnboardingStep::NestProvisioning);
    assert!(m.dns_config().set_up_later);
    assert_eq!(
        m.vps_config().selected_server_type_id.as_deref(),
        Some("cx23"),
        "the default type is chosen on Continue"
    );
    assert!(m.provisioning_start_blocked_reason().is_none());

    m.clone().start_provisioning();
    for _ in 0..1500 {
        if !matches!(
            m.provisioning_snapshot().overall,
            OverallStatus::Idle | OverallStatus::Running
        ) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        m.provisioning_snapshot().overall,
        OverallStatus::Succeeded,
        "{:?} / {:?}",
        m.provisioning_snapshot().final_error,
        m.error_message()
    );
    let requests = server.received_requests().await.expect("recorded");
    let create = requests
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/servers")
        .expect("one create_server");
    let auth = create
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        auth,
        Some("Bearer tok"),
        "the connected credential rents the server"
    );
    assert!(
        !m.provisioning_choices_visible(),
        "the choices hide once the run succeeded"
    );
}

// ── The page's step: entered only by an app that renders it ────────────────

async fn continue_from_handle_check(m: &OnboardingMachine) -> OnboardingStep {
    m.set_step_for_test(OnboardingStep::HandleEntry);
    m.set_handle_check_outcome_for_test(HandleCheckOutcome::DomainAvailable {
        buyable_via_provider: true,
        price: None,
    });
    m.submit_handle_check_continue().await
}

#[tokio::test]
async fn the_handle_check_lands_on_the_page_only_on_an_app_that_renders_it() {
    let m = machine_for(BUY_HANDLE);
    assert_eq!(
        continue_from_handle_check(&m).await,
        OnboardingStep::DnsConfig,
        "an app that has not built the page keeps dns_config"
    );
    let m = machine_for(BUY_HANDLE);
    m.set_renders_provider_choice(true);
    assert_eq!(
        continue_from_handle_check(&m).await,
        OnboardingStep::ProviderChoice
    );
    assert_eq!(m.step(), OnboardingStep::ProviderChoice);
    assert!(
        m.dns_config().buy_domain,
        "buy_domain is still machine-derived"
    );
    m.back();
    assert_eq!(
        m.step(),
        OnboardingStep::HandleEntry,
        "Back → the handle check"
    );
}

#[test]
fn back_from_the_build_page_returns_to_the_page_on_its_path() {
    let m = machine_for("alice@example.com");
    m.set_step_for_test(OnboardingStep::ProviderChoice);
    m.set_provider_connection_for_test("hetzner".into(), connected(&["example.com"], true));
    m.continue_from_providers().unwrap();
    assert_eq!(m.step(), OnboardingStep::NestProvisioning);
    m.back();
    assert_eq!(m.step(), OnboardingStep::ProviderChoice);

    // Cloud recovery and the old path still return to vps_config.
    let m = machine_for("alice@example.com");
    m.set_step_for_test(OnboardingStep::NestProvisioning);
    m.back();
    assert_eq!(m.step(), OnboardingStep::VpsConfig);
}

#[tokio::test]
async fn a_reset_keeps_the_apps_declared_page() {
    let m = machine_for(BUY_HANDLE);
    m.set_renders_provider_choice(true);
    m.reset();
    m.begin_create_identity();
    m.set_current_handle(BUY_HANDLE.into());
    assert_eq!(
        continue_from_handle_check(&m).await,
        OnboardingStep::ProviderChoice,
        "a capability declared once survives a factory reset"
    );
}

#[test]
fn the_goal_lines_name_the_handles_domain() {
    let m = machine_for("alice@example.com");
    assert_eq!(m.provider_goal_domain(), "example.com");
}
