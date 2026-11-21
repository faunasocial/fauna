//! The **choose-providers** page's model (`docs/goal/behavior/onboarding-provisioning.md`
//! § 3d, third round 2026-11-16 — a bought domain's DNS may run at another
//! connected provider): the goal set of the run (registration while
//! a domain is bought, DNS, a server), the connected-provider set, the one
//! *Connect* that runs every check a provider's capabilities allow, the
//! can-cover rule with its stated reason, the goal assignment (fewest
//! providers), the Continue predicate, and the defaults Continue writes before
//! it lands on `nest_provisioning` — plus that page's side of it: the choices
//! region's visibility, the re-chosen server type and the start predicate. All
//! of it here, so an app renders rows, a dialog and up to three goal lines from
//! getters and forwards presses — no app computes coverage or a default
//! (priority #2).
//!
//! On Continue the covers are written into the `dns` / `vps` state the
//! provisioning run already reads, so `run_provisioning_inner`, the Bill of
//! Materials and `captured_dns_credential()` are untouched. The old
//! `dns_config` / `vps_config` provider methods stay beside this until the page
//! lands on every app.

use std::collections::HashMap;

use fauna_core::secret::SecretString;
use fauna_provisioning::progress::OverallStatus;
use fauna_provisioning::registrar::{ContactInfo, RegistrarAvailability};
use fauna_provisioning::vps::ServerTypeInfo;
use fauna_provisioning::{Capability, PROVIDERS, ProviderMeta};

use crate::error::OnboardingError;
use crate::machine::{HostedAuthSlot, OnboardingMachine, covering_zone, dispatch_provider_id};
use crate::state::{
    CapabilityPlain, FieldMetaPlain, GoalCover, GoalRow, HeldProviderCredential, HostedAuthPrompt,
    HostedAuthState, LocalizedText, OnboardingStep, ProviderConnection, ProviderGoal, ProviderRow,
    State,
};

// Draft wording until the page's public-texts request (§ 3d).
const KEY_NO_DNS: &str = "onboarding.provider_choice.cannot_cover_no_dns";
const KEY_NO_SERVER: &str = "onboarding.provider_choice.cannot_cover_no_server";
const KEY_NO_REGISTRAR: &str = "onboarding.provider_choice.cannot_cover_no_registrar";
const KEY_NOT_REGISTRABLE: &str = "onboarding.provider_choice.cannot_cover_not_registrable";
const KEY_ZONE_MISSING: &str = "onboarding.provider_choice.cannot_cover_zone_missing";
const KEY_NO_ZONE_DOOR: &str = "onboarding.provider_choice.cannot_cover_no_zone_door";
const KEY_NO_NS_DOOR: &str = "onboarding.provider_choice.cannot_cover_no_nameserver_door";
const KEY_POINTED_ELSEWHERE: &str = "onboarding.provider_choice.cannot_cover_pointed_elsewhere";
const KEY_NOT_POINTED_YET: &str = "onboarding.provider_choice.dns_not_pointed_yet";
const KEY_STATUS_DOMAIN: &str = "onboarding.provider_choice.status_cover_domain";
const KEY_STATUS_DNS: &str = "onboarding.provider_choice.status_cover_dns";
const KEY_STATUS_VPS: &str = "onboarding.provider_choice.status_cover_vps";
const KEY_STATUS_CONTACT: &str = "onboarding.provider_choice.status_contact_missing";
const KEY_START_PRICE: &str = "onboarding.provider_choice.start_blocked_price";
const KEY_START_TYPE: &str = "onboarding.provider_choice.start_blocked_server_type";
const KEY_REFUSED_NOT_BUYING: &str = "onboarding.provider_choice.refused_not_buying";
const KEY_REFUSED_FIXED: &str = "onboarding.provider_choice.refused_registration_fixed";
const KEY_REFUSED_NOT_CONNECTED: &str = "onboarding.provider_choice.refused_not_connected";

/// The manual row's id in [`ProviderRow::provider_id`] (`provider-row[manual]`).
pub const MANUAL_ROW_ID: &str = "manual";

fn meta(provider_id: &str) -> Option<&'static ProviderMeta> {
    PROVIDERS.iter().find(|p| p.id.as_str() == provider_id)
}

/// The `{provider}` argument of a can-cover reason or the DNS goal's note: the
/// provider's display-name key, so the app resolves the sentence with
/// `resolve_nested` and it names the provider as its row does, never by its
/// id (an id the registry does not know is passed through as itself).
fn provider_arg(provider_id: &str) -> String {
    meta(provider_id).map_or_else(
        || provider_id.to_string(),
        |p| p.display_name_key.to_string(),
    )
}

fn capability_plain(c: &Capability) -> CapabilityPlain {
    match c {
        Capability::Dns => CapabilityPlain::Dns,
        Capability::Vps => CapabilityPlain::Vps,
        Capability::Registrar => CapabilityPlain::Registrar,
        Capability::Zone => CapabilityPlain::Zone,
    }
}

fn requires_contact(p: &ProviderMeta) -> bool {
    p.registrar_requires_contact == Some(true)
}

/// The default server type on the setting-things-up page: the cheapest offer
/// the mail gate permits (`server_type_allowed_for_mail`), the first of equal
/// price. `None` when no offer is permitted.
pub(crate) fn default_server_type(types: &[ServerTypeInfo], mail: bool) -> Option<String> {
    types
        .iter()
        .filter(|t| crate::helpers::server_type_allowed_for_mail((*t).clone(), mail))
        .min_by_key(|t| t.price_monthly_cents)
        .map(|t| t.id.clone())
}

/// What the cover computation reads beyond the connected set: the handle's
/// domain, whether it is being bought (or this wizard bought it), which
/// provider covers the registration goal, the wizard-life memory, and the
/// domain's delegation as the handle check read it.
struct CoverInputs {
    domain: Option<String>,
    buying: bool,
    domain_cover: Option<String>,
    /// The registrar this wizard registered the handle's domain at.
    registered: Option<String>,
    /// The provider this wizard pointed the registered domain at.
    pointed_at: Option<String>,
    /// `State::handle_delegation`.
    delegation: Vec<String>,
}

/// The wizard-life memory's registrar, while it names the handle's domain.
/// This run buys a domain it has not registered yet, so its price must be
/// agreed before *Buy and set up*. On a run entered from the choose-providers
/// page that is the registration cover's — whatever covers DNS, the manual row
/// included, since the deferred-DNS run still registers (§ 6 *Deferred-DNS
/// path*); on the `dns_config` path, a DNS provider chosen to buy through.
pub(crate) fn run_buys_domain(s: &State) -> bool {
    s.dns.buy_domain
        && if s.providers.provisioning_from_providers {
            s.providers.run_registrar.is_some() && registered_registrar(s).is_none()
        } else {
            !s.dns.set_up_later && s.dns.selected_provider_id.is_some()
        }
}

pub(crate) fn registered_registrar(s: &State) -> Option<String> {
    let domain = s.handle_domain()?;
    s.providers
        .registered
        .as_ref()
        .filter(|r| r.domain == domain)
        .map(|r| r.registrar.clone())
}

/// A connected provider holds a zone of exactly the domain's name whose
/// assigned nameservers the domain's delegation names none of — a left-over,
/// or the zone of a run that stopped before pointing the domain (§ 3d → *A
/// zone the domain does not point at*).
fn not_pointed_at(conn: &ProviderConnection, inputs: &CoverInputs) -> bool {
    use fauna_provisioning::probe::normalize_nameserver;
    let Some(domain) = inputs.domain.as_deref() else {
        return false;
    };
    conn.zones.iter().any(|z| {
        z.name == domain
            && !z.nameservers.is_empty()
            && !z
                .nameservers
                .iter()
                .any(|ns| inputs.delegation.contains(&normalize_nameserver(ns)))
    })
}

/// **The can-cover rule — the one computation behind both the verdict and its
/// reason** (the `dns_provider_eligible` / `dns_provider_ineligible_reason`
/// template). `None` = this provider covers the goal (when connected; for an
/// unconnected provider, `None` means it could once connected). `Some(key)` =
/// why it cannot. A capability shortfall reads the same connected or not; an
/// account-based one (the zone, the registrar's quote) only once connected.
fn cannot_cover(
    p: &ProviderMeta,
    conn: Option<&ProviderConnection>,
    goal: ProviderGoal,
    inputs: &CoverInputs,
) -> Option<&'static str> {
    let connected = conn.filter(|c| c.connected);
    match goal {
        ProviderGoal::Domain => {
            if !p.capabilities.contains(&Capability::Registrar) {
                return Some(KEY_NO_REGISTRAR);
            }
            // Bought by this wizard: its registrar covers the goal, fixed.
            if let Some(r) = &inputs.registered {
                return (r != p.id.as_str()).then_some(KEY_NOT_REGISTRABLE);
            }
            // No DNS capability needed: DNS can run at another provider.
            match connected?.availability {
                Some(RegistrarAvailability::Buyable { .. }) => None,
                _ => Some(KEY_NOT_REGISTRABLE),
            }
        }
        ProviderGoal::Dns => {
            if !p.capabilities.contains(&Capability::Dns) {
                return Some(KEY_NO_DNS);
            }
            if inputs.buying {
                // Pointed by this run: that provider's zone answers, no other.
                if let Some(pointed) = &inputs.pointed_at {
                    return (pointed != p.id.as_str()).then_some(KEY_POINTED_ELSEWHERE);
                }
                // (1) The registrar itself, publishing into its own zone.
                match &inputs.domain_cover {
                    Some(d) if d == p.id.as_str() => return None,
                    None if cannot_cover(p, conn, ProviderGoal::Domain, inputs).is_none() => {
                        return None;
                    }
                    _ => {}
                }
                // (2) Another provider: it creates the zone, the registration
                // cover points the domain at it.
                if !p.dns_creates_zones {
                    return Some(KEY_NO_ZONE_DOOR);
                }
                if let Some(d) = inputs.domain_cover.as_deref()
                    && !meta(d).is_some_and(|r| r.registrar_sets_nameservers)
                {
                    return Some(KEY_NO_NS_DOOR);
                }
                return None;
            }
            let c = connected?;
            let holds_zone = inputs
                .domain
                .as_deref()
                .is_some_and(|d| covering_zone(d, &c.zones).is_some());
            if holds_zone {
                None
            } else {
                Some(KEY_ZONE_MISSING)
            }
        }
        ProviderGoal::Vps => {
            if !p.capabilities.contains(&Capability::Vps) {
                return Some(KEY_NO_SERVER);
            }
            // A held credential without the server fields connected DNS only.
            match connected {
                Some(c) if !c.server_checked => Some(KEY_NO_SERVER),
                _ => None,
            }
        }
    }
}

/// [`cannot_cover`]'s reason as the text a surface paints, its arguments
/// filled — the one form the provider rows and a refused pick both use.
fn cannot_cover_text(
    p: &ProviderMeta,
    conn: Option<&ProviderConnection>,
    goal: ProviderGoal,
    inputs: &CoverInputs,
) -> Option<LocalizedText> {
    cannot_cover(p, conn, goal, inputs).map(|key| match key {
        KEY_ZONE_MISSING => {
            LocalizedText::key_arg(key, "domain", inputs.domain.clone().unwrap_or_default())
        }
        KEY_NO_NS_DOOR => LocalizedText::key_arg(
            key,
            "provider",
            provider_arg(inputs.domain_cover.as_deref().unwrap_or_default()),
        ),
        KEY_POINTED_ELSEWHERE => LocalizedText::key_arg(
            key,
            "provider",
            provider_arg(inputs.pointed_at.as_deref().unwrap_or_default()),
        ),
        _ => LocalizedText::key(key),
    })
}

/// The connected providers that can cover `goal`, in registry order.
fn candidates(s: &State, goal: ProviderGoal, inputs: &CoverInputs) -> Vec<&'static str> {
    PROVIDERS
        .iter()
        .filter(|p| {
            let conn = s.providers.providers.get(p.id.as_str());
            conn.is_some_and(|c| c.connected) && cannot_cover(p, conn, goal, inputs).is_none()
        })
        .map(|p| p.id.as_str())
        .collect()
}

/// The registration goal's cover while buying: the registrar this wizard
/// bought it at, fixed; else the person's choice where it still holds, else a
/// candidate that also covers the server, else the first.
fn domain_cover(s: &State, inputs: &CoverInputs) -> Option<String> {
    if !inputs.buying {
        return None;
    }
    let dom_c = candidates(s, ProviderGoal::Domain, inputs);
    if inputs.registered.is_some() {
        return dom_c.first().map(|p| p.to_string());
    }
    let vps_c = candidates(s, ProviderGoal::Vps, inputs);
    s.providers
        .domain_choice
        .as_deref()
        .filter(|p| dom_c.contains(p))
        .or_else(|| dom_c.iter().find(|p| vps_c.contains(p)).copied())
        .or_else(|| dom_c.first().copied())
        .map(str::to_string)
}

/// The effective cover of every goal on the page.
struct Covers {
    /// `None` while not buying too (the goal is not on the page).
    domain: Option<String>,
    dns: Option<GoalCover>,
    vps: Option<String>,
}

/// The person's DNS choice, where it still holds: the manual row always, a
/// provider while it is a candidate.
fn dns_choice(s: &State, dns_c: &[&str]) -> Option<GoalCover> {
    match &s.providers.dns_choice {
        Some(GoalCover::Manual) => Some(GoalCover::Manual),
        Some(GoalCover::Provider { provider_id }) if dns_c.contains(&provider_id.as_str()) => {
            Some(GoalCover::Provider {
                provider_id: provider_id.clone(),
            })
        }
        _ => None,
    }
}

fn provider_of(c: &Option<GoalCover>) -> Option<&str> {
    match c {
        Some(GoalCover::Provider { provider_id }) => Some(provider_id.as_str()),
        _ => None,
    }
}

/// The assignment, preferring the fewest providers: the person's choice where
/// it still holds, else one that covers every goal before two — the
/// registration cover for DNS, the DNS cover for the server — and, bringing a
/// domain, a provider whose zone the domain's delegation names before one it
/// does not.
fn covers(s: &State, inputs: &CoverInputs) -> Covers {
    let vps_c = candidates(s, ProviderGoal::Vps, inputs);
    let vps_choice = s
        .providers
        .vps_choice
        .clone()
        .filter(|p| vps_c.contains(&p.as_str()));
    let dns_c = candidates(s, ProviderGoal::Dns, inputs);

    if inputs.buying {
        let domain = inputs.domain_cover.clone();
        let dns = dns_choice(s, &dns_c).or_else(|| {
            domain
                .as_deref()
                .filter(|p| dns_c.contains(p))
                .or(dns_c.first().copied())
                .map(|p| GoalCover::Provider {
                    provider_id: p.to_string(),
                })
        });
        let vps = vps_choice.or_else(|| {
            domain
                .as_deref()
                .filter(|p| vps_c.contains(p))
                .or(provider_of(&dns).filter(|p| vps_c.contains(p)))
                .or(vps_c.first().copied())
                .map(str::to_string)
        });
        return Covers { domain, dns, vps };
    }

    // Bringing the domain: prefer the zones it is delegated to.
    let pointed: Vec<&str> = dns_c
        .iter()
        .copied()
        .filter(|p| {
            !s.providers
                .providers
                .get(*p)
                .is_some_and(|c| not_pointed_at(c, inputs))
        })
        .collect();
    let pool = if pointed.is_empty() { &dns_c } else { &pointed };
    let both = pool.iter().find(|p| vps_c.contains(p)).copied();
    let dns_choice = dns_choice(s, &dns_c);
    let vps = vps_choice.or_else(|| {
        provider_of(&dns_choice)
            .filter(|p| vps_c.contains(p))
            .or(both)
            .or(vps_c.first().copied())
            .map(str::to_string)
    });
    let dns = dns_choice.or_else(|| {
        vps.as_deref()
            .filter(|p| pool.contains(p))
            .or(both)
            .or(pool.first().copied())
            .map(|p| GoalCover::Provider {
                provider_id: p.to_string(),
            })
    });
    Covers {
        domain: None,
        dns,
        vps,
    }
}

/// Why Continue is dead, or `None`: the first open goal in page order, then a
/// covering registrar still missing the contact it requires.
fn continue_blocked(s: &State, inputs: &CoverInputs, c: &Covers) -> Option<LocalizedText> {
    if inputs.buying && c.domain.is_none() {
        return Some(LocalizedText::key_arg(
            KEY_STATUS_DOMAIN,
            "domain",
            inputs.domain.clone().unwrap_or_default(),
        ));
    }
    if c.dns.is_none() {
        return Some(LocalizedText::key(KEY_STATUS_DNS));
    }
    if c.vps.is_none() {
        return Some(LocalizedText::key(KEY_STATUS_VPS));
    }
    if let Some(pid) = c.domain.as_deref()
        && meta(pid).is_some_and(requires_contact)
        && s.providers
            .providers
            .get(pid)
            .is_none_or(|conn| conn.contact.is_none())
    {
        return Some(LocalizedText::key_arg(
            KEY_STATUS_CONTACT,
            "provider",
            provider_arg(pid),
        ));
    }
    None
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl OnboardingMachine {
    /// The fields a provider's dialog shows: DNS and server fields always,
    /// registrar fields when the domain is being bought — so a provider is
    /// connected once, whole. Empty for an unknown id.
    pub fn visible_provider_fields(&self, provider_id: String) -> Vec<FieldMetaPlain> {
        let Some(p) = meta(&provider_id) else {
            return vec![];
        };
        let buying = self.with_state(|s| s.dns.buy_domain);
        let show_registrar = buying && p.capabilities.contains(&Capability::Registrar);
        p.fields
            .iter()
            .filter(|f| {
                f.kinds.contains(&Capability::Dns)
                    || f.kinds.contains(&Capability::Vps)
                    || (show_registrar && f.kinds.contains(&Capability::Registrar))
            })
            .map(FieldMetaPlain::from)
            .collect()
    }

    /// One field of a provider's dialog. A changed credential un-connects the
    /// provider until *Connect* runs again (as `set_dns_cred` un-verifies).
    pub fn set_provider_cred(&self, provider_id: String, field_id: String, value: String) {
        self.mutate(|s| {
            let c = s.providers.providers.entry(provider_id).or_default();
            c.creds.insert(field_id, SecretString::from(value));
            c.connected = false;
            c.held = false;
        });
    }

    /// [`Self::connect_provider_possibility`] as the bare flag; kept public
    /// for the apps that still read it until their trickle-down lands.
    pub fn can_connect_provider(&self, provider_id: String) -> bool {
        self.connect_provider_possibility(provider_id).is_possible()
    }

    /// `provider-connect-button`'s answer (`ui/README.md` § Offer only what is
    /// possible, rules 2 and 6): no connect of this provider already on its
    /// way, and every required visible field filled. An unknown provider has
    /// no dialog, so no button: absent. [`Self::connect_provider`] asks it
    /// first.
    pub fn connect_provider_possibility(
        &self,
        provider_id: String,
    ) -> fauna_core::possibility::Possibility {
        use crate::credential_gate::{CONNECTING, FILL_REQUIRED, first_blocked, required_filled};
        let visible = self.visible_provider_fields(provider_id.clone());
        if visible.is_empty() {
            return fauna_core::possibility::Possibility::Absent;
        }
        self.with_state(|s| {
            let conn = s.providers.providers.get(&provider_id);
            first_blocked(&[
                (conn.is_some_and(|c| c.connecting), CONNECTING),
                (
                    !required_filled(&visible, |id| {
                        conn.and_then(|c| c.creds.get(id))
                            .is_some_and(|v| !v.is_empty())
                    }),
                    FILL_REQUIRED,
                ),
            ])
        })
    }

    /// *Disconnect*: drop the provider's credential from the wizard; any goal
    /// it covered is uncovered (and reassigned when another provider can).
    pub fn disconnect_provider(&self, provider_id: String) {
        self.mutate(|s| {
            s.providers.providers.remove(&provider_id);
            if s.providers.domain_choice.as_deref() == Some(provider_id.as_str()) {
                s.providers.domain_choice = None;
            }
            if matches!(&s.providers.dns_choice,
                Some(GoalCover::Provider { provider_id: p }) if *p == provider_id)
            {
                s.providers.dns_choice = None;
            }
            if s.providers.vps_choice.as_deref() == Some(provider_id.as_str()) {
                s.providers.vps_choice = None;
            }
        });
    }

    /// The person's pick on a goal row (`provider-goal-select`), or the manual
    /// row for DNS (always allowed, a bought domain included). Refused for a
    /// cover that cannot cover the goal: the manual row on registration or the
    /// server; a pick on registration while nothing is bought, or once this
    /// wizard bought the domain (the goal is fixed); a provider that is not
    /// connected or cannot cover it. A refusal leaves its reason on the
    /// shared `error_message` (`ui/README.md` § Offer only what is possible,
    /// rule 7), so an app that drops the `Err` still shows why; an accepted
    /// pick clears it.
    pub fn select_goal_cover(
        &self,
        goal: ProviderGoal,
        cover: GoalCover,
    ) -> Result<(), OnboardingError> {
        if let Some(why) = self.goal_cover_refusal(&goal, &cover) {
            return Err(self.refuse_with("provider_choice", why));
        }
        self.mutate(|s| match (goal, cover) {
            (ProviderGoal::Dns, c) => s.providers.dns_choice = Some(c),
            (ProviderGoal::Domain, GoalCover::Provider { provider_id }) => {
                s.providers.domain_choice = Some(provider_id)
            }
            (ProviderGoal::Vps, GoalCover::Provider { provider_id }) => {
                s.providers.vps_choice = Some(provider_id)
            }
            (_, GoalCover::Manual) => {}
        });
        self.clear_error();
        Ok(())
    }

    /// A pick's answer (`docs/goal/ui/README.md` § Offer only what is
    /// possible, rule 6): the same refusal [`Self::select_goal_cover`] asks
    /// first, as one value — so the manual row (DNS only, always possible
    /// for DNS) and each provider's pick are painted from the machine and
    /// never from an app reading a row's `dns_reason` itself.
    pub fn goal_cover_possibility(
        &self,
        goal: ProviderGoal,
        cover: GoalCover,
    ) -> fauna_core::possibility::Possibility {
        fauna_core::possibility::Possibility::from_blocked_reason(
            self.goal_cover_refusal(&goal, &cover),
        )
    }

    /// `provider-choice-continue-button`'s answer in the shared possibility
    /// type — [`Self::providers_continue_blocked_reason`] as one value, the
    /// answer [`Self::continue_from_providers`] asks first.
    pub fn providers_continue_possibility(&self) -> fauna_core::possibility::Possibility {
        fauna_core::possibility::Possibility::from_blocked_reason(
            self.providers_continue_blocked_reason(),
        )
    }

    /// `provisioning-start-button`'s answer: on a run entered from the
    /// choose-providers page, [`Self::provisioning_start_blocked_reason`]
    /// (the price not agreed, no server type); on the older vps_config path,
    /// always possible. The routing guard each app used to add itself
    /// ([`Self::provisioning_back_to_providers`]) is folded in here, and
    /// `start_provisioning` refuses on the same answer.
    pub fn provisioning_start_possibility(&self) -> fauna_core::possibility::Possibility {
        fauna_core::possibility::Possibility::from_blocked_reason(
            if self.provisioning_back_to_providers() {
                self.provisioning_start_blocked_reason()
            } else {
                None
            },
        )
    }

    /// One row per registry provider, registry order, then the manual row
    /// ([`MANUAL_ROW_ID`], no capabilities, never connected), each with its
    /// per-goal cannot-cover reason from the one can-cover computation. The
    /// registration reason is `None` while nothing is bought.
    pub fn provider_rows(&self) -> Vec<ProviderRow> {
        let inputs = self.cover_inputs();
        self.with_state(|s| {
            let mut rows: Vec<ProviderRow> = PROVIDERS
                .iter()
                .map(|p| {
                    let conn = s.providers.providers.get(p.id.as_str());
                    let reason = |goal| cannot_cover_text(p, conn, goal, &inputs);
                    ProviderRow {
                        provider_id: p.id.as_str().to_string(),
                        capabilities: p.capabilities.iter().map(capability_plain).collect(),
                        connected: conn.is_some_and(|c| c.connected),
                        connecting: conn.is_some_and(|c| c.connecting),
                        held: conn.is_some_and(|c| c.connected && c.held),
                        error: conn.and_then(|c| c.error.clone()),
                        domain_reason: if inputs.buying {
                            reason(ProviderGoal::Domain)
                        } else {
                            None
                        },
                        dns_reason: reason(ProviderGoal::Dns),
                        vps_reason: reason(ProviderGoal::Vps),
                    }
                })
                .collect();
            // *I will set DNS up later*: DNS only, and always — for a bought
            // domain the run registers it and publishes nothing.
            rows.push(ProviderRow {
                provider_id: MANUAL_ROW_ID.to_string(),
                capabilities: vec![],
                connected: false,
                connecting: false,
                held: false,
                error: None,
                domain_reason: inputs.buying.then(|| LocalizedText::key(KEY_NO_REGISTRAR)),
                dns_reason: None,
                vps_reason: Some(LocalizedText::key(KEY_NO_SERVER)),
            });
            rows
        })
    }

    /// The goal lines in page order: registration (only while buying, with
    /// the covering registrar's price), DNS, the server.
    pub fn goal_rows(&self) -> Vec<GoalRow> {
        let inputs = self.cover_inputs();
        self.with_state(|s| {
            let c = covers(s, &inputs);
            let names = |goal| {
                candidates(s, goal, &inputs)
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            };
            let row = |goal, cover| GoalRow {
                goal,
                cover,
                candidates: names(goal),
                price_cents: None,
                renewal_cents: None,
                currency: None,
                note: None,
            };
            let mut rows = Vec::with_capacity(3);
            if inputs.buying {
                let quote = c
                    .domain
                    .as_deref()
                    .and_then(|p| s.providers.providers.get(p))
                    .and_then(|conn| match &conn.availability {
                        Some(RegistrarAvailability::Buyable {
                            price_cents,
                            currency,
                            renewal_cents,
                        }) => Some((*price_cents, *renewal_cents, currency.clone())),
                        _ => None,
                    });
                let mut domain = row(
                    ProviderGoal::Domain,
                    c.domain
                        .clone()
                        .map(|provider_id| GoalCover::Provider { provider_id }),
                );
                if let Some((price, renewal, currency)) = quote {
                    domain.price_cents = Some(price);
                    domain.renewal_cents = renewal;
                    domain.currency = currency;
                }
                rows.push(domain);
            }
            let mut dns = row(ProviderGoal::Dns, c.dns.clone());
            if !inputs.buying
                && let Some(pid) = provider_of(&c.dns)
                && s.providers
                    .providers
                    .get(pid)
                    .is_some_and(|conn| not_pointed_at(conn, &inputs))
            {
                dns.note = Some(LocalizedText::key_args(
                    KEY_NOT_POINTED_YET,
                    [
                        ("domain", inputs.domain.clone().unwrap_or_default()),
                        ("provider", provider_arg(pid)),
                    ],
                ));
            }
            rows.push(dns);
            rows.push(row(
                ProviderGoal::Vps,
                c.vps.map(|provider_id| GoalCover::Provider { provider_id }),
            ));
            rows
        })
    }

    /// Continue is live: every goal on the page is covered and the covering
    /// registrar has the contact it requires.
    pub fn can_continue_providers(&self) -> bool {
        self.providers_continue_blocked_reason().is_none()
    }

    /// Why Continue is dead, naming the first open goal in page order or the
    /// registrar whose contact is missing (`ui/README.md` § Copy
    /// comprehensibility rule 5) — `None` when it is live. Paired with
    /// [`Self::can_continue_providers`] over one computation.
    pub fn providers_continue_blocked_reason(&self) -> Option<LocalizedText> {
        let inputs = self.cover_inputs();
        self.with_state(|s| continue_blocked(s, &inputs, &covers(s, &inputs)))
    }

    /// Whether `dns-buy-domain-checkbox` is offered: exactly when the handle
    /// check found the domain unregistered (`DomainAvailable`, either
    /// reading) and this wizard has not registered it since — a registered
    /// domain cannot be bought.
    pub fn can_offer_buy_domain(&self) -> bool {
        matches!(
            self.domain_status(),
            Some(fauna_provisioning::probe::DomainStatus::Unregistered)
        ) && self.with_state(|s| registered_registrar(s).is_none())
    }

    /// The domain the goal lines name (*Register `<domain>`*, *DNS for
    /// `<domain>`*): the handle's domain, empty before a handle is typed.
    pub fn provider_goal_domain(&self) -> String {
        self.with_state(|s| s.handle_domain()).unwrap_or_default()
    }

    /// The dialog shows the WHOIS contact form (`dns-contact-form`): a domain
    /// is bought, the provider is connected, quotes it as buyable and its
    /// registrar `requires_contact()`.
    pub fn provider_shows_contact_form(&self, provider_id: String) -> bool {
        let Some(p) = meta(&provider_id) else {
            return false;
        };
        requires_contact(p)
            && self.with_state(|s| {
                s.dns.buy_domain
                    && s.providers.providers.get(&provider_id).is_some_and(|c| {
                        c.connected
                            && matches!(c.availability, Some(RegistrarAvailability::Buyable { .. }))
                    })
            })
    }

    /// The registrar's note for the dialog (`dns-registrar-notes-text`) while
    /// a domain is bought: the registry's `registrar_notes_key`, for a
    /// registrar that uses the contact on its own account.
    pub fn provider_registrar_notes_key(&self, provider_id: String) -> Option<String> {
        let p = meta(&provider_id)?;
        if !self.with_state(|s| s.dns.buy_domain)
            || !p.capabilities.contains(&Capability::Registrar)
        {
            return None;
        }
        p.registrar_notes_key.map(str::to_string)
    }

    /// The contact form's values: the account's default contact *Connect*
    /// fetched, or what the person entered.
    pub fn provider_contact(&self, provider_id: String) -> Option<ContactInfo> {
        self.with_state(|s| {
            s.providers
                .providers
                .get(&provider_id)
                .and_then(|c| c.contact.clone())
        })
    }

    /// The contact form's submit; Continue writes the registration cover's
    /// contact into `dns` state.
    pub fn set_provider_contact(&self, provider_id: String, contact: ContactInfo) {
        self.mutate(|s| {
            s.providers
                .providers
                .entry(provider_id)
                .or_default()
                .contact = Some(contact);
        });
    }

    /// Continue: write the covering providers, their credentials, the contact
    /// and the defaults (the server's default location with its offers
    /// listed, the cheapest offer the mail gate permits) into the `dns` /
    /// `vps` state the provisioning run already reads, then land on
    /// `nest_provisioning` — nothing is bought or changed here, and the
    /// price is agreed there. A manual DNS cover takes the deferred-DNS path.
    pub fn continue_from_providers(&self) -> Result<(), OnboardingError> {
        let inputs = self.cover_inputs();
        let checked = self.with_state(|s| {
            let c = covers(s, &inputs);
            continue_blocked(s, &inputs, &c).map_or(Ok(c), Err)
        });
        let c = match checked {
            Ok(c) => c,
            Err(why) => return Err(self.refuse_with("provider_choice", why)),
        };
        let (Some(dns), Some(vps)) = (c.dns, c.vps) else {
            unreachable!("continue_blocked names an open goal");
        };
        let mail = self.provision_mail_mode_enabled();
        let mut listed_for = None;
        self.mutate(|s| {
            // The covers decide which provider serves which goal; the
            // same-provider mirror in `continue_from_dns` must not overwrite
            // the server state written here.
            s.dns.same_provider_for_vps = false;
            match &dns {
                GoalCover::Manual => {
                    s.dns.set_up_later = true;
                    s.dns.selected_provider_id = None;
                    s.dns.creds.clear();
                    s.dns.hosted_auth.clear();
                    s.dns.verified = false;
                    s.dns.zone_id = None;
                    s.dns.current_zones.clear();
                    s.dns.current_availability = None;
                }
                GoalCover::Provider { provider_id } => {
                    let conn = s.providers.providers[provider_id].clone();
                    let zone_id = inputs
                        .domain
                        .as_deref()
                        .and_then(|d| covering_zone(d, &conn.zones).map(|z| z.id.clone()))
                        .or_else(|| conn.zones.first().map(|z| z.id.clone()));
                    s.dns.set_up_later = false;
                    s.dns.selected_provider_id = Some(provider_id.clone());
                    s.dns.creds = conn.creds;
                    s.dns.hosted_auth = conn.hosted_auth;
                    s.dns.verified = true;
                    s.dns.zone_id = zone_id;
                    s.dns.current_zones = conn.zones;
                    s.dns.current_availability = conn.availability;
                    // While buying, the registration cover's contact is the
                    // one the run registers with.
                    if inputs.buying || s.dns.contact.is_none() {
                        s.dns.contact = conn.contact;
                    }
                }
            }
            let conn = s.providers.providers[&vps].clone();
            let location = conn
                .listed_for_location
                .clone()
                .or_else(|| conn.locations.first().map(|l| l.id.clone()));
            s.vps.selected_provider_id = Some(vps.clone());
            s.vps.creds = conn.creds;
            s.vps.hosted_auth = conn.hosted_auth;
            s.vps.verified = true;
            s.vps.locations = conn.locations;
            s.vps.selected_location_id = location;
            let keep = s.vps.selected_server_type_id.as_deref().is_some_and(|id| {
                conn.server_types.iter().any(|t| {
                    t.id == id && crate::helpers::server_type_allowed_for_mail(t.clone(), mail)
                })
            });
            if !keep {
                s.vps.selected_server_type_id = default_server_type(&conn.server_types, mail);
            }
            s.vps.server_types = conn.server_types;
            listed_for = conn.listed_for_location;
            // The registration goal's cover registers the domain; with DNS at
            // another provider (or the manual row) it is not the DNS cover.
            s.providers.run_registrar = c.domain.clone();
            if let Some(reg) = c.domain.as_deref() {
                let conn = s.providers.providers[reg].clone();
                s.dns.current_availability = conn.availability;
                s.dns.contact = conn.contact;
            }
            s.providers.provisioning_from_providers = true;
            s.error_message = None;
            s.step = OnboardingStep::NestProvisioning;
        });
        self.note_vps_offers_listed_for(listed_for);
        self.spawn_channel_probe();
        self.spawn_dns_plan_refresh();
        Ok(())
    }

    /// `nest_provisioning` shows its choices region (location, server type,
    /// mail mode, update channel, SSH door): the run was entered from this
    /// page's Continue — not from cloud recovery, which chose on `vps_config`
    /// — and has not started or succeeded (Retry after a failed or cancelled
    /// run reads the choices afresh).
    pub fn provisioning_choices_visible(&self) -> bool {
        self.with_state(|s| s.providers.provisioning_from_providers)
            && !matches!(
                self.provisioning_snapshot().overall,
                OverallStatus::Running | OverallStatus::Succeeded
            )
    }

    /// Why *Buy and set up* is dead, for the un-id'd line beneath it — `None`
    /// when it is live: a domain bought through a provider whose price is not
    /// yet agreed (`dns-price-confirm-checkbox`), or no server type selected.
    /// `start_provisioning` refuses while it is `Some` on a run entered from
    /// this page (the `vps_config` path enforces both at its own Continues).
    pub fn provisioning_start_blocked_reason(&self) -> Option<LocalizedText> {
        self.with_state(|s| {
            if run_buys_domain(s) && !s.dns.price_agreed {
                return Some(LocalizedText::key(KEY_START_PRICE));
            }
            if s.vps.selected_server_type_id.is_none() {
                return Some(LocalizedText::key(KEY_START_TYPE));
            }
            None
        })
    }

    /// `dns-price-confirm-checkbox` is painted under the domain line: the
    /// choices region shows, this run buys a domain it has not registered yet
    /// ([`run_buys_domain`] — on the manual DNS cover too, which still
    /// registers) and the registrar's quote gives the line its price. The one
    /// predicate every app reads; none re-derives it from the DNS config.
    pub fn provisioning_price_confirm_visible(&self) -> bool {
        self.provisioning_choices_visible()
            && self.with_state(run_buys_domain)
            && self.bill_of_materials().iter().any(|i| !i.recurring)
    }

    /// Back from `nest_provisioning` returns to the choose-providers page
    /// (`true`) rather than `vps_config` — the routing decision as data until
    /// the page's `OnboardingStep` lands with the page itself.
    pub fn provisioning_back_to_providers(&self) -> bool {
        self.with_state(|s| s.providers.provisioning_from_providers)
    }

    /// Where a `hosted-auth` field of a provider's dialog stands (the button
    /// label source, as [`Self::hosted_auth_state`] is for the built forms).
    pub fn provider_hosted_auth_state(
        &self,
        provider_id: String,
        field_id: String,
    ) -> HostedAuthState {
        self.slot_hosted_auth_state(&HostedAuthSlot::Provider(provider_id), &field_id)
    }

    /// Whether a provider dialog's sign-in button is pressable.
    pub fn provider_hosted_auth_can_begin(&self, provider_id: String, field_id: String) -> bool {
        self.slot_hosted_auth_can_begin(&HostedAuthSlot::Provider(provider_id), &field_id)
    }

    /// A provider dialog's sign-in button's answer, with its reason.
    pub fn provider_hosted_auth_possibility(
        &self,
        provider_id: String,
        field_id: String,
    ) -> fauna_core::possibility::Possibility {
        self.slot_hosted_auth_possibility(&HostedAuthSlot::Provider(provider_id), &field_id)
    }

    /// The sign-in button's label for one field of a provider's dialog.
    pub fn provider_hosted_auth_button_text(
        &self,
        provider_id: String,
        field_id: String,
    ) -> String {
        crate::hosted_auth::button_text(&self.provider_hosted_auth_state(provider_id, field_id))
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl OnboardingMachine {
    /// *Connect*: run every check the provider's capabilities allow with the
    /// one credential set — the DNS check that lists the account's zones, the
    /// registrar's quote for an unregistered domain, the server check that
    /// lists locations and the offers of the first — and mark it connected.
    /// A failed check leaves it unconnected with the provider's own error.
    pub async fn connect_provider(&self, provider_id: String) -> Result<(), OnboardingError> {
        if !self
            .connect_provider_possibility(provider_id.clone())
            .is_possible()
        {
            return Err(OnboardingError::InvalidTransition {
                from: "provider_choice".into(),
                reason: format!("{provider_id}: fill in every field first"),
            });
        }
        self.connect_inner(&provider_id, false).await
    }

    /// Sign in at the provider for one `hosted-auth` field of its dialog
    /// (the device-authorization flow [`Self::hosted_auth_begin`] runs).
    pub async fn provider_hosted_auth_begin(
        &self,
        provider_id: String,
        field_id: String,
    ) -> Result<HostedAuthPrompt, OnboardingError> {
        self.slot_hosted_auth_begin(HostedAuthSlot::Provider(provider_id), field_id)
            .await
    }

    /// Wait for that sign-in; the token lands in the dialog's credential.
    pub async fn provider_hosted_auth_wait(
        &self,
        provider_id: String,
        field_id: String,
    ) -> Result<(), OnboardingError> {
        self.slot_hosted_auth_wait(HostedAuthSlot::Provider(provider_id), field_id)
            .await
    }

    /// The credentials signed-in accounts on this device hold (an add-account
    /// run), passed in by the app. Each is re-checked: one whose checks pass
    /// reads connected (held); one that fails is dropped, so a revoked token
    /// reads as not connected. A provider the person already connected in this
    /// run is left alone. The server check runs only when the provider's
    /// server fields are among the fields held.
    pub async fn seed_held_provider_credentials(&self, held: Vec<HeldProviderCredential>) {
        for h in held {
            if meta(&h.provider_id).is_none() {
                continue;
            }
            let fresh = self.mutate(|s| {
                if s.providers.providers.contains_key(&h.provider_id) {
                    return false;
                }
                s.providers.providers.insert(
                    h.provider_id.clone(),
                    ProviderConnection {
                        creds: h.fields.clone(),
                        held: true,
                        ..Default::default()
                    },
                );
                true
            });
            if fresh && self.connect_inner(&h.provider_id, true).await.is_err() {
                self.mutate(|s| {
                    s.providers.providers.remove(&h.provider_id);
                });
            }
        }
    }

    /// The binding faces' form of [`Self::seed_held_provider_credentials`]: the
    /// index-paired provider ids and JSON field bags `retire_held_inputs` reads
    /// off the account's DNS record — the same pair `set_held_dns_json` takes on
    /// the retire page — so no app projects the credential bag itself.
    pub async fn seed_held_provider_credentials_json(
        &self,
        provider_ids: Vec<String>,
        creds_json: Vec<String>,
    ) {
        let held = crate::retire::held_dns_from_json(provider_ids, creds_json)
            .into_iter()
            .map(Into::into)
            .collect();
        self.seed_held_provider_credentials(held).await;
    }
}

impl OnboardingMachine {
    /// Why a pick of `cover` for `goal` is refused, or `None` when it is
    /// accepted — the one computation behind [`Self::select_goal_cover`] and
    /// [`Self::goal_cover_possibility`]. Outside the `uniffi::export` block:
    /// it is the machine's own, not a door.
    fn goal_cover_refusal(&self, goal: &ProviderGoal, cover: &GoalCover) -> Option<LocalizedText> {
        let goal = *goal;
        let cover = cover.clone();
        let inputs = self.cover_inputs();
        let domain = || inputs.domain.clone().unwrap_or_default();
        self.with_state(|s| match (&goal, &cover) {
            (ProviderGoal::Vps, GoalCover::Manual) => Some(LocalizedText::key(KEY_STATUS_VPS)),
            (ProviderGoal::Domain, GoalCover::Manual) => Some(LocalizedText::key_arg(
                KEY_STATUS_DOMAIN,
                "domain",
                domain(),
            )),
            (ProviderGoal::Domain, _) if !inputs.buying => {
                Some(LocalizedText::key(KEY_REFUSED_NOT_BUYING))
            }
            (ProviderGoal::Domain, _) if inputs.registered.is_some() => {
                let mut why = LocalizedText::key_arg(KEY_REFUSED_FIXED, "domain", domain());
                why.args.insert(
                    "provider".into(),
                    provider_arg(inputs.registered.as_deref().unwrap_or_default()),
                );
                Some(why)
            }
            (_, GoalCover::Manual) => None,
            (_, GoalCover::Provider { provider_id }) => {
                if candidates(s, goal, &inputs).contains(&provider_id.as_str()) {
                    return None;
                }
                let conn = s.providers.providers.get(provider_id.as_str());
                meta(provider_id)
                    .and_then(|p| cannot_cover_text(p, conn, goal, &inputs))
                    .or_else(|| {
                        Some(LocalizedText::key_arg(
                            KEY_REFUSED_NOT_CONNECTED,
                            "provider",
                            provider_arg(provider_id),
                        ))
                    })
            }
        })
    }

    fn cover_inputs(&self) -> CoverInputs {
        self.with_state(|s| {
            let registered = registered_registrar(s);
            let mut inputs = CoverInputs {
                domain: s.handle_domain(),
                buying: s.dns.buy_domain || registered.is_some(),
                domain_cover: None,
                pointed_at: registered
                    .as_ref()
                    .and_then(|_| s.providers.pointed_at.clone()),
                registered,
                delegation: s.handle_delegation.clone(),
            };
            inputs.domain_cover = domain_cover(s, &inputs);
            inputs
        })
    }

    /// The checks behind *Connect*. `held`: a seeded credential, whose server
    /// check runs only when its server fields are present.
    async fn connect_inner(&self, provider_id: &str, held: bool) -> Result<(), OnboardingError> {
        use fauna_provisioning::dispatch;
        use fauna_provisioning::dns::DnsProvider;
        use fauna_provisioning::probe::DomainStatus;
        use fauna_provisioning::registrar::Registrar;
        use fauna_provisioning::vps::VpsProvider;

        let Some(p) = meta(provider_id) else {
            return Err(OnboardingError::Other {
                detail: format!("unknown provider id: {provider_id}"),
            });
        };
        let Some(pid) = dispatch_provider_id(provider_id) else {
            return Err(OnboardingError::Other {
                detail: format!("unknown provider id: {provider_id}"),
            });
        };
        let creds: HashMap<String, SecretString> = self.mutate(|s| {
            let c = s
                .providers
                .providers
                .entry(provider_id.to_string())
                .or_default();
            c.connecting = true;
            c.error = None;
            c.creds.clone()
        });
        let fail = |msg: String| {
            self.mutate(|s| {
                if let Some(c) = s.providers.providers.get_mut(provider_id) {
                    c.connecting = false;
                    c.connected = false;
                    c.error = Some(msg.clone());
                }
            });
            OnboardingError::Other { detail: msg }
        };

        let mut zones = vec![];
        if p.capabilities.contains(&Capability::Dns) {
            let Some(d) = dispatch::dns_provider(
                pid,
                dispatch::Credentials::from_map(creds.clone()),
                self.provider_base_url("dns".into()),
            ) else {
                return Err(fail(format!("provider {provider_id} doesn't support DNS")));
            };
            zones = d
                .verify(&self.http)
                .await
                .map_err(|e| fail(format!("Verify failed: {e}")))?;
        }

        // The registrar's quote and default contact — non-fatal, as in
        // `verify_dns`: a missing quote reads as "cannot register".
        let mut availability = None;
        let mut contact = None;
        if let Some(reg) = dispatch::registrar(pid, dispatch::Credentials::from_map(creds.clone()))
        {
            let domain = self.with_state(|s| s.handle_domain());
            if matches!(self.domain_status(), Some(DomainStatus::Unregistered))
                && let Some(domain) = domain.as_deref()
                && let Ok(av) = reg.availability(&self.http, domain).await
            {
                availability = Some(av);
            }
            if reg.requires_contact()
                && let Ok(Some(c)) = reg.fetch_default_contact(&self.http).await
            {
                contact = Some(c);
            }
        }

        let has_server_fields = p
            .fields
            .iter()
            .filter(|f| f.required && f.kinds.contains(&Capability::Vps))
            .all(|f| creds.get(f.id).is_some_and(|v| !v.is_empty()));
        let run_server = p.capabilities.contains(&Capability::Vps) && (!held || has_server_fields);
        let mut server = None;
        if run_server {
            let Some(v) = dispatch::vps_provider(
                pid,
                dispatch::Credentials::from_map(creds.clone()),
                self.provider_base_url("vps".into()),
            ) else {
                return Err(fail(format!("provider {provider_id} doesn't support VPS")));
            };
            let locations = v
                .verify(&self.http)
                .await
                .map_err(|e| fail(format!("Verify failed: {e}")))?;
            let location = locations.first().map(|l| l.id.clone());
            let offers = v
                .list_server_types(
                    &self.http,
                    p.curated_offers,
                    location.as_deref().unwrap_or_default(),
                )
                .await
                .map_err(|e| fail(format!("Server type list failed: {e}")))?;
            server = Some((locations, offers.into_iter().take(5).collect(), location));
        }

        self.mutate(|s| {
            let c = s
                .providers
                .providers
                .entry(provider_id.to_string())
                .or_default();
            c.connecting = false;
            c.connected = true;
            c.error = None;
            c.zones = zones;
            c.availability = availability;
            c.contact = contact;
            c.server_checked = server.is_some();
            if let Some((locations, offers, location)) = server {
                c.locations = locations;
                c.server_types = offers;
                c.listed_for_location = location;
            }
        });
        Ok(())
    }
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl OnboardingMachine {
    /// The e2e machine bridge's arms for this page (`call_machine_method` and
    /// its two siblings consult it first). `None` = not one of this page's
    /// names; `Some(v)` = handled, `v` the reader's JSON value (`None` for a
    /// command). Argument shapes follow the bridge's conventions: a bare id as
    /// a JSON string, several arguments as a JSON array.
    pub(crate) fn provider_choice_bridge(
        &self,
        name: &str,
        json_arg: &str,
    ) -> Option<Option<String>> {
        fn id(json_arg: &str) -> String {
            serde_json::from_str::<String>(json_arg).unwrap_or_else(|_| json_arg.to_string())
        }
        fn parse<T: serde::de::DeserializeOwned>(name: &str, json_arg: &str) -> Option<T> {
            serde_json::from_str(json_arg)
                .map_err(|error| tracing::warn!(method = name, %error, "malformed bridge argument"))
                .ok()
        }
        fn json<T: serde::Serialize>(v: &T) -> Option<String> {
            serde_json::to_string(v).ok()
        }
        Some(match name {
            "set_provider_cred" => {
                if let Some((p, f, v)) = parse::<(String, String, String)>(name, json_arg) {
                    self.set_provider_cred(p, f, v);
                }
                None
            }
            "disconnect_provider" => {
                self.disconnect_provider(id(json_arg));
                None
            }
            "select_goal_cover" => {
                if let Some((goal, cover)) = parse::<(ProviderGoal, GoalCover)>(name, json_arg) {
                    let _ = self.select_goal_cover(goal, cover);
                }
                None
            }
            "continue_from_providers" => {
                let _ = self.continue_from_providers();
                None
            }
            "set_provider_connection_for_test" => {
                if let Some((p, conn)) = parse::<(String, ProviderConnection)>(name, json_arg) {
                    self.set_provider_connection_for_test(p, conn);
                }
                None
            }
            "provider_rows" => return Some(json(&self.provider_rows())),
            "goal_rows" => return Some(json(&self.goal_rows())),
            "visible_provider_fields" => {
                return Some(json(&self.visible_provider_fields(id(json_arg))));
            }
            "can_continue_providers" => return Some(json(&self.can_continue_providers())),
            "providers_continue_blocked_reason" => {
                return Some(json(&self.providers_continue_blocked_reason()));
            }
            "set_provider_contact" => {
                if let Some((p, contact)) = parse::<(String, ContactInfo)>(name, json_arg) {
                    self.set_provider_contact(p, contact);
                }
                None
            }
            "provider_contact" => return Some(json(&self.provider_contact(id(json_arg)))),
            "provider_shows_contact_form" => {
                return Some(json(&self.provider_shows_contact_form(id(json_arg))));
            }
            "provider_registrar_notes_key" => {
                return Some(json(&self.provider_registrar_notes_key(id(json_arg))));
            }
            "can_offer_buy_domain" => return Some(json(&self.can_offer_buy_domain())),
            "provisioning_choices_visible" => {
                return Some(json(&self.provisioning_choices_visible()));
            }
            "provisioning_price_confirm_visible" => {
                return Some(json(&self.provisioning_price_confirm_visible()));
            }
            "provisioning_start_blocked_reason" => {
                return Some(json(&self.provisioning_start_blocked_reason()));
            }
            "provisioning_back_to_providers" => {
                return Some(json(&self.provisioning_back_to_providers()));
            }
            _ => return None,
        })
    }

    /// The async arms: *Connect* and the held-credential seed. The ack waits
    /// for the effect, as `verify_dns`'s arm does.
    pub(crate) async fn provider_choice_bridge_async(
        &self,
        name: &str,
        json_arg: &str,
    ) -> Option<Option<String>> {
        match name {
            "connect_provider" => {
                let p = serde_json::from_str::<String>(json_arg)
                    .unwrap_or_else(|_| json_arg.to_string());
                let _ = self.connect_provider(p).await;
                Some(None)
            }
            "seed_held_provider_credentials" => {
                match serde_json::from_str::<Vec<HeldProviderCredential>>(json_arg) {
                    Ok(held) => self.seed_held_provider_credentials(held).await,
                    Err(error) => {
                        tracing::warn!(method = name, %error, "malformed bridge argument")
                    }
                }
                Some(None)
            }
            _ => None,
        }
    }

    /// Test-only: put one provider into the connected-provider set as its
    /// checks would have left it, without standing up its API.
    pub fn set_provider_connection_for_test(&self, provider_id: String, conn: ProviderConnection) {
        self.mutate(|s| {
            s.providers.providers.insert(provider_id, conn);
        });
    }

    /// Test-only: the handle check's reading of the domain's delegation.
    pub fn set_handle_delegation_for_test(&self, nameservers: Vec<String>) {
        self.mutate(|s| s.handle_delegation = nameservers);
    }

    /// Test-only: the wizard-life memory as a run's events would leave it — a
    /// registration of the handle's domain at `registrar`, and the provider
    /// the run pointed it at.
    pub fn set_wizard_memory_for_test(
        &self,
        registrar: Option<String>,
        pointed_at: Option<String>,
    ) {
        self.mutate(|s| {
            s.providers.registered = registrar.and_then(|registrar| {
                Some(crate::state::RegisteredDomain {
                    domain: s.handle_domain()?,
                    registrar,
                })
            });
            s.providers.pointed_at = pointed_at;
        });
    }
}
