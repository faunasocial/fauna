//! The admin unified-DNS sub-page (`admin-dns`) — the one place that answers "is
//! my deployment's DNS correct, and what do I do about it?"
//! (`dns-management.md` § App surface).
//!
//! **Two shared machines on one page** — the split is the page's whole structure:
//!
//! - `fauna-client-dns`'s `DnsManagementMachine` owns the DNS half: the per-domain
//!   *record matrix* with its live public-DNS red/green verdicts, the client-held
//!   provider-credential store, and the per-domain managed/manual opt-in. The nest
//!   is **read/verify only** here (`dns-management.md:151`) — the credential never
//!   leaves the admin's clients.
//! - `fauna-client-mail-settings`'s `LocalDomainMachine` owns the *domain rows*:
//!   add / remove / restore, the per-domain catch-all + role-address designations,
//!   and the primary-domain rename lifecycle (`admin.md:176`).
//!
//! So the **rows come from the local-domain snapshot and the records are looked up
//! per row by name** from the DNS snapshot — linux's `render_admin_dns` shape, and
//! the reason a domain the admin just added renders immediately (its row exists)
//! with an empty record list until the next matrix read.
//!
//! **tui has no modals, so every confirm and every form on this page is an inline
//! reveal** painted into this page's own element list: the add-domain form, the
//! write-only add-credential form, the rename sheet, and the rename banner's
//! complete/abort confirm pairs. That is not cosmetic — the shared e2e asserts
//! `is_visible` on a revealed element *synchronously* after the click that armed
//! it, and an async-opening dialog loses that race (`admin/bridges.rs`'s rotate
//! confirm learned this first).
//!
//! A dumb renderer: the shell (`super`) owns both machines, their ops and their
//! folds; every projection is the shared machine's (`DnsSnapshot::
//! all_domains_managed`, `DomainView::is_managed`, `LocalDomainsSnapshot::
//! rename_available`, the rename view's own `can_*` affordance flags), never
//! re-derived here.
//!
//! Scope: Phase 1 — the core page. The per-domain TLS-cert lifecycle
//! (`admin-dns-cert-*`, `admin-dns-domain-auto-renew`; `tls-certificates.md`
//! § C.3 / § C.4 / § B tier 3) is Phase 2 and is declared unbuilt in
//! `ui-actual-tui.yaml` until it lands.

use fauna_client_dns::{CertStatusRow, DelegationView, DomainView, PendingCertIssue};
use fauna_client_mail_settings::local_domains::{
    LocalDomainView, LocalDomainsSnapshot, RoleAddressKind,
};
use fauna_i18n::strings::admin as t_admin;
use fauna_i18n::strings::admin::dns as t;
use fauna_i18n::strings::common;
use fauna_provisioning::{Capability, FieldType, PROVIDERS};
use fauna_ui_ids as ids;

use super::{Action, AdminField, AdminState, credential_zones, rename_target_names};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

/// The DNS-capable field ids of `provider_id`, in `providers.yaml` order — the
/// exact set the write-only add-credential form collects and `PutCredentials`
/// verifies. Each id is **also** the element id its entry is tagged with (the
/// cross-app shape: the driver types into `api-token` directly, linux
/// `views/admin.rs::rebuild_credential_fields`), so this one list serves both the
/// paint and the submit and they cannot disagree about which fields exist.
pub(super) fn credential_field_ids(provider_id: &str) -> Vec<String> {
    PROVIDERS
        .iter()
        .find(|p| p.id.as_str() == provider_id)
        .map(|p| {
            p.fields
                .iter()
                .filter(|f| f.kinds.contains(&Capability::Dns))
                .map(|f| f.id.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// An empty [`Element`] never registers with the frame, so a per-row field that
/// can render empty would silently drop that row from its own id's index
/// sequence — leaving the driver's positional `index=i` addressing a *different*
/// domain (or record) than the name it matched on. Every optional per-row and
/// per-record field goes through here, so each row contributes exactly one
/// element per id. Same guard, same reason as `admin/bridges.rs::or_placeholder`;
/// this page's optional columns (a record with no verdict yet, a domain with no
/// rename state) are exactly that shape.
fn or_dash(text: String) -> String {
    if text.is_empty() {
        "—".to_string()
    } else {
        text
    }
}

/// The record's live public-DNS verdict as the admin reads it: the shared
/// verdict enum's own label, with the observed value appended on a mismatch so
/// the admin sees *what* is wrong without leaving the page. `None` (not yet
/// verified) reads as "Checking…" — never a false green or red.
fn record_status_text(row: &fauna_client_dns::DnsRecordRow) -> String {
    use fauna_client_dns::VerifyStatus;
    // The verdict → label-key decision is the shared one
    // (`fauna_core::format::dns_verdict_label`, `value-formatting.md` § DNS
    // verdict label), reached the same way this page's own `cert_status_text`
    // reaches `cert_status_view`: map the typed enum to its serde variant name,
    // because `fauna_core` does not depend on `fauna-client-dns`. Only the
    // resolve is ours. An absent verdict falls through the shared `_` arm to
    // "Checking…" — the neutral no-verdict state, never a false green/red.
    let variant = match row.verdict.as_ref().map(|v| v.status) {
        Some(VerifyStatus::Ok) => "Ok",
        Some(VerifyStatus::Missing) => "Missing",
        Some(VerifyStatus::Mismatch) => "Mismatch",
        Some(VerifyStatus::Checking) | None => "Checking",
    };
    let observed = row
        .verdict
        .as_ref()
        .map(|v| v.observed.as_slice())
        .unwrap_or(&[]);
    fauna_core::format::dns_verdict_label(variant, observed).resolve(fauna_i18n::strings::lookup)
}

/// The per-domain served-cert health badge (`tls-certificates.md` § C.4).
/// Owned by `fauna_client_dns::cert_status_text` — see its doc comment for
/// the shared decision + text-assembly split.
use fauna_client_dns::cert_status_text;

/// The actor-picker options for a per-domain designation: the clear sentinel
/// first (index 0 — the localized "None" / "Admin (default)", which is what
/// `resolve_dns_actor` maps back to *no* designation), then one option per
/// listed actor. A current designation the actor list does not carry (paginated
/// out, or the user was removed) gets a **trailing fallback option** so it stays
/// visible and selected — linux's `actor_id_fallback_label` behaviour, and the
/// reason a stale designation can't read as an accidental clear.
fn actor_options(
    state: &AdminState,
    clear_label: &str,
    current: Option<&[u8]>,
) -> (Vec<String>, String) {
    let mut options = Vec::with_capacity(state.dns_actors.len() + 2);
    options.push(clear_label.to_string());
    options.extend(state.dns_actors.iter().map(|(_, label)| label.clone()));

    let selected = match current {
        None => clear_label.to_string(),
        Some(current) => match state.dns_actors.iter().find(|(id, _)| id == current) {
            Some((_, label)) => label.clone(),
            None => {
                let full = fauna_core::format::hex_full(current);
                let fallback = t_admin::actor_id_fallback_label(&full);
                options.push(fallback.clone());
                fallback
            }
        },
    };
    (options, selected)
}

pub(super) fn dns_elements(state: &AdminState) -> Vec<Element> {
    let dns = state.dns_snapshot.as_ref();
    let domains = state.local_domains_snapshot.as_ref();

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::chrome(t::DESCRIPTION),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t_admin::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
        Element::gesture_button(
            ids::ADMIN_DNS_REFRESH_BUTTON,
            t::REFRESH,
            true,
            Gesture::Admin(Action::RefreshDns),
        ),
    ];

    // ── The deployment master switch ────────────────────────────────────────
    //
    // "Fauna controls all domains" — a convenience that sets every active
    // domain's mode at once; the stored state stays per-domain
    // (`dns-management.md` § The two modes). Its checked state is the SHARED
    // `all_domains_managed` fold over the active rows, never a local re-count.
    let active_names: Vec<String> = domains
        .map(|s| s.active.iter().map(|d| d.domain.clone()).collect())
        .unwrap_or_default();
    let all_managed = dns
        .map(|s| s.all_domains_managed(Some(&active_names)))
        .unwrap_or(false);
    els.push(Element::checkbox_gesture(
        ids::ADMIN_DNS_MANAGE_ALL_TOGGLE,
        t::MANAGE_ALL,
        all_managed,
        Gesture::Admin(Action::ToggleDnsManageAll {
            managed: !all_managed,
        }),
    ));

    // ── Held DNS-provider credentials (client-held; multi) ──────────────────
    els.push(Element::label(
        ids::ADMIN_DNS_CREDENTIALS_LIST,
        t::CREDENTIALS_TITLE,
    ));
    let credentials = dns.map(|s| s.credentials.as_slice()).unwrap_or_default();
    if credentials.is_empty() {
        els.push(Element::chrome(t::CREDENTIALS_EMPTY));
    }
    for (i, cred) in credentials.iter().enumerate() {
        // The item marker is the indexed component anchor; the fields below it
        // paint in ui.yaml order, so `index=i` lines up across every id.
        els.push(
            Element::label(ids::ADMIN_DNS_CREDENTIAL_ITEM, cred.label.clone())
                .within(ids::ADMIN_DNS_CREDENTIALS_LIST, 0),
        );
        els.push(
            Element::label(
                ids::ADMIN_DNS_CREDENTIAL_ITEM_PROVIDER,
                cred.provider_id.clone(),
            )
            .within(ids::ADMIN_DNS_CREDENTIALS_LIST, 0),
        );
        els.push(
            Element::label(
                ids::ADMIN_DNS_CREDENTIAL_ITEM_ZONES,
                // A verified credential always reports at least one zone, but a
                // zero-zone row must still contribute its element or every later
                // row's index shifts under the driver.
                or_dash(cred.zones.join(", ")),
            )
            .labelled(t::CREDENTIAL_ZONES)
            .within(ids::ADMIN_DNS_CREDENTIALS_LIST, 0),
        );
        els.push(
            Element::gesture_button(
                ids::ADMIN_DNS_CREDENTIAL_ITEM_CLEAR_BUTTON,
                t::REMOVE,
                true,
                Gesture::Admin(Action::ClearDnsCredential { index: i as u32 }),
            )
            .within(ids::ADMIN_DNS_CREDENTIALS_LIST, 0),
        );
    }

    // ── The write-only add-credential form (inline reveal) ──────────────────
    //
    // Mirrors onboarding's `dns_config` provider-row + per-field-entry pattern
    // (priority #3 — same concepts, same ids) but self-contained: no
    // `OnboardingMachine`, the shared `DnsManagementMachine` verifies and seals.
    if state.dns_add_credential_open {
        // `admin-dns-add-credential-provider-row` is the *container*; each button
        // is `admin-dns-add-credential-provider-row[<pid>]` — a literal bracket in
        // the id (the cross-app shape), NOT a positional scope index.
        els.push(Element::label(
            ids::ADMIN_DNS_ADD_CREDENTIAL_PROVIDER_ROW,
            "",
        ));
        for p in PROVIDERS
            .iter()
            .filter(|p| p.capabilities.contains(&Capability::Dns))
        {
            let id = p.id.as_str();
            els.push(
                Element::checkbox_gesture(
                    format!("admin-dns-add-credential-provider-row[{id}]"),
                    crate::wizard::key(p.display_name_key),
                    state.dns_credential_provider.as_deref() == Some(id),
                    Gesture::Admin(Action::SelectDnsCredentialProvider(id.to_string())),
                )
                .within(ids::ADMIN_DNS_ADD_CREDENTIAL_PROVIDER_ROW, 0),
            );
        }
        // The field-entry container; its entries exist only once a provider is
        // picked, and each is tagged with the RAW providers.yaml field id.
        els.push(Element::label(ids::ADMIN_DNS_ADD_CREDENTIAL_FORM, ""));
        if let Some(provider_id) = state.dns_credential_provider.as_deref() {
            let secret_ids: Vec<&str> = PROVIDERS
                .iter()
                .find(|p| p.id.as_str() == provider_id)
                .map(|p| {
                    p.fields
                        .iter()
                        .filter(|f| {
                            f.kinds.contains(&Capability::Dns)
                                && matches!(f.field_type, FieldType::Secret | FieldType::HostedAuth)
                        })
                        .map(|f| f.id)
                        .collect()
                })
                .unwrap_or_default();
            for field_id in credential_field_ids(provider_id) {
                let label = PROVIDERS
                    .iter()
                    .find(|p| p.id.as_str() == provider_id)
                    .and_then(|p| p.fields.iter().find(|f| f.id == field_id))
                    .map(|f| crate::wizard::key(f.label_key))
                    .unwrap_or_else(|| field_id.clone());
                // A Secret field is marked so the input paints masked — the value
                // itself is never read back (the form is write-only; the held
                // list shows provider + zones only).
                let secret = secret_ids.contains(&field_id.as_str());
                els.push(
                    Element::input(
                        field_id.clone(),
                        state
                            .dns_credential_fields
                            .get(&field_id)
                            .cloned()
                            .unwrap_or_default(),
                        Field::Admin(AdminField::DnsCredential(field_id.clone())),
                    )
                    .labelled(label)
                    .attr("secret", if secret { "true" } else { "false" })
                    .within(ids::ADMIN_DNS_ADD_CREDENTIAL_FORM, 0),
                );
            }
        }
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_ADD_CREDENTIAL_SUBMIT_BUTTON,
            t::ADD_CREDENTIAL_SUBMIT,
            state.dns_credential_provider.is_some(),
            Gesture::Admin(Action::SubmitDnsCredential),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_ADD_CREDENTIAL_CANCEL_BUTTON,
            common::CANCEL,
            true,
            Gesture::Admin(Action::CancelDnsAddCredential),
        ));
    } else {
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_ADD_CREDENTIAL_BUTTON,
            t::ADD_CREDENTIAL,
            true,
            Gesture::Admin(Action::OpenDnsAddCredential),
        ));
    }

    // ── The add-domain form (inline reveal) ─────────────────────────────────
    //
    // This page is the single domain-management surface (the former read-only
    // `admin-settings` "Email domains" section is removed — `admin.md:176`).
    if state.dns_add_domain_open {
        els.push(
            Element::input(
                ids::ADMIN_DNS_ADD_DOMAIN_INPUT,
                state.dns_add_domain_input.clone(),
                Field::Admin(AdminField::DnsAddDomain),
            )
            .labelled(t::ADD_DOMAIN_PLACEHOLDER),
        );
        // A domainless nest's first add is a one-way door — say so before the
        // submit that makes it irreversible (`deployment-home-with-public-relay.md`
        // § MUA reach; `mail-multidomain.md` § Removing a local domain). The nest,
        // not this client, is authority on whether the add is actually first —
        // `adding_first_domain` is a UX hint off the already-fetched domain list.
        if domains.is_some_and(|s| s.adding_first_domain) {
            els.push(Element::chrome(t::ADD_DOMAIN_PRIMARY_WARNING));
        }
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_ADD_DOMAIN_SUBMIT_BUTTON,
            t::ADD_DOMAIN_SUBMIT,
            !state.dns_add_domain_input.trim().is_empty(),
            Gesture::Admin(Action::SubmitDnsAddDomain),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_ADD_DOMAIN_CANCEL_BUTTON,
            common::CANCEL,
            true,
            Gesture::Admin(Action::CancelDnsAddDomain),
        ));
    } else {
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_ADD_DOMAIN_BUTTON,
            t::ADD_DOMAIN,
            true,
            Gesture::Admin(Action::OpenDnsAddDomain),
        ));
    }

    // ── The deployment-wide in-flight rename banner ─────────────────────────
    if let Some(rename) = domains.and_then(|s| s.active_rename.as_ref()) {
        els.extend(rename_banner_elements(state, rename));
    }

    // ── The start-a-rename wizard sheet (inline reveal) ─────────────────────
    if state.dns_rename_sheet_open {
        els.extend(rename_sheet_elements(state, domains));
    }

    // ── One section per active domain ───────────────────────────────────────
    let active: &[LocalDomainView] = domains.map(|s| s.active.as_slice()).unwrap_or_default();
    if active.is_empty() {
        els.push(Element::chrome(t::EMPTY));
        els.push(Element::chrome(t::EMPTY_DESC));
    }
    let rename_available = domains.map(|s| s.rename_available).unwrap_or(false);
    let active_rename = domains.and_then(|s| s.active_rename.as_ref());
    // The single in-flight manual-paste issuance, if any: the machine holds at
    // most one (`snapshot.pending_cert`, re-projected from the persisted
    // breadcrumb by any machine with no live order — `tls-certificates.md`
    // § Surviving an interrupted manual issuance), so its `domain` selects which
    // section renders the paste surface.
    let pending_cert = dns.and_then(|s| s.pending_cert.as_ref());
    for (i, row) in active.iter().enumerate() {
        els.extend(domain_section(
            state,
            i,
            row,
            dns.and_then(|s| s.domains.iter().find(|d| d.domain == row.domain)),
            dns.and_then(|s| s.cert_statuses.iter().find(|c| c.domain == row.domain)),
            dns.and_then(|s| s.delegations.iter().find(|d| d.domain == row.domain)),
            pending_cert,
            rename_available,
            active_rename,
        ));
    }

    // ── Soft-deleted domains (30-day restore window) ────────────────────────
    let removed: &[LocalDomainView] = domains
        .map(|s| s.soft_deleted.as_slice())
        .unwrap_or_default();
    if !removed.is_empty() {
        els.push(Element::chrome(t::REMOVED_TITLE));
        els.push(Element::chrome(t::REMOVED_DESC));
    }
    for row in removed {
        els.push(Element::label(
            ids::ADMIN_DNS_REMOVED_DOMAIN,
            row.domain.clone(),
        ));
        els.push(Element::label(
            ids::ADMIN_DNS_REMOVED_DOMAIN_NAME,
            row.domain.clone(),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_REMOVED_DOMAIN_RESTORE_BUTTON,
            t::RESTORE,
            true,
            Gesture::Admin(Action::RestoreDnsDomain {
                domain: row.domain.clone(),
            }),
        ));
    }

    els
}

/// This page's error, in linux's precedence: domain-CRUD feedback first (it is
/// the more recent, more specific act — e.g. `cannot_remove_primary_domain`),
/// then the list/verify read failure.
///
/// Read by `App::screen_error_text` rather than painted here: the page's error
/// has to reach the ONE funnel that feeds the paint, the registry and the state
/// protocol's `messages.error` alike (`App::error_line_text`'s honesty
/// contract). A page that pushes its own `error-message` element instead is
/// registered but unreadable — see `crate::admin::page_error`.
pub(super) fn page_error(state: &AdminState) -> Option<String> {
    state
        .dns_action_error
        .as_deref()
        .or_else(|| {
            state
                .local_domains_snapshot
                .as_ref()
                .and_then(|s| s.error.as_deref())
        })
        .or_else(|| state.dns_snapshot.as_ref().and_then(|s| s.error.as_deref()))
        .filter(|e| !e.is_empty())
        .map(str::to_string)
}

/// One `admin-dns-domain` section: the row's own affordances, then its record
/// matrix. Every child is scoped `.within(ids::ADMIN_DNS_DOMAIN, i)` so a
/// `scope="admin-dns-domain[i]"` query resolves to exactly this domain's copy of
/// a repeated id (the Phase-2 cert affordances need that; the flat positional
/// `index=i` reads keep working either way because the sections paint in order).
#[allow(clippy::too_many_arguments)]
fn domain_section(
    state: &AdminState,
    i: usize,
    row: &LocalDomainView,
    matrix: Option<&DomainView>,
    cert: Option<&CertStatusRow>,
    delegation: Option<&DelegationView>,
    pending_cert: Option<&PendingCertIssue>,
    rename_available: bool,
    active_rename: Option<
        &fauna_client_mail_settings::primary_domain_rename::PrimaryDomainRenameView,
    >,
) -> Vec<Element> {
    let scope = |el: Element| el.within(ids::ADMIN_DNS_DOMAIN, i);
    let mut els = vec![
        Element::label(ids::ADMIN_DNS_DOMAIN, row.domain.clone()),
        scope(Element::label(
            ids::ADMIN_DNS_DOMAIN_NAME,
            row.domain.clone(),
        )),
    ];

    // The mode control's TEXT is the mode label ("Fauna-managed" / "Manual") —
    // the cross-app contract the driver reads back with `get_text`
    // (`actions/admin.py::dns_domain_modes`). Managed-ness is the shared
    // `DomainView::is_managed` projection over the machine's effective-mode
    // overlay (opted-in ∧ a held credential covers the domain), never a local
    // `mode == "managed"` string match. No matrix row yet ⇒ manual.
    let managed = matrix.map(|d| d.is_managed()).unwrap_or(false);
    els.push(scope(Element::checkbox_gesture(
        ids::ADMIN_DNS_DOMAIN_MODE,
        if managed {
            t::MODE_MANAGED
        } else {
            t::MODE_MANUAL
        },
        managed,
        Gesture::Admin(Action::ToggleDnsDomainMode {
            domain: row.domain.clone(),
            managed: !managed,
        }),
    )));

    // Auto-renew (`tls-certificates.md` § C.3): shown ONLY for a managed or
    // delegated domain, because only those can auto-issue — a
    // manual-non-delegated domain's `auto_renew` is `false` regardless, so
    // painting the control there would offer a toggle that cannot take effect.
    // Default is ON, so the admin only ever touches it to disable hands-off
    // renewal. The checked state rides the `state` attr because the label is the
    // constant "Auto-renew" (the driver reads `get_attr(id, "state")`).
    let delegated = delegation.is_some();
    if managed || delegated {
        els.push(scope(
            Element::checkbox_gesture(
                ids::ADMIN_DNS_DOMAIN_AUTO_RENEW,
                t::cert::AUTO_RENEW,
                matrix.is_some_and(|d| d.auto_renew),
                Gesture::Admin(Action::ToggleDnsAutoRenew {
                    domain: row.domain.clone(),
                    enabled: !matrix.is_some_and(|d| d.auto_renew),
                }),
            )
            .attr(
                "state",
                if matrix.is_some_and(|d| d.auto_renew) {
                    "on"
                } else {
                    "off"
                },
            ),
        ));
    }

    // Per-domain catch-all actor ("None" clears) — a 1-per-domain routing
    // setting, so it is a field on this row rather than a section of its own
    // (`admin.md:181`).
    let (catch_all_options, catch_all_selected) =
        actor_options(state, t::CATCH_ALL_NONE, row.catch_all_actor_id.as_deref());
    els.push(
        scope(Element::select(
            ids::ADMIN_DNS_DOMAIN_CATCH_ALL_SELECT,
            catch_all_selected,
            SelectTarget::DnsCatchAll { row: i },
            catch_all_options,
        ))
        .labelled(t::CATCH_ALL_LABEL),
    );
    // Present only when a SUCCESSION (not an admin) last cleared this
    // domain's catch-all — tells the admin why the picker above
    // reads "none" and that unmatched mail is now bouncing; re-designating
    // via that same picker is the fix. Same conditional-read-only-label
    // idiom as the rename-state line below.
    if row.catch_all_cleared_by_succession_at.is_some() {
        els.push(scope(Element::label(
            ids::ADMIN_DNS_DOMAIN_CATCH_ALL_CLEARED_STATE,
            t::CATCH_ALL_CLEARED_BY_SUCCESSION,
        )));
    }

    // The four RFC 2142 role-address overrides ("Admin (default)" clears). The
    // nest atomic-merges, so each role is independent.
    for role in RoleAddressKind::ALL {
        let current = row
            .role_address_overrides
            .iter()
            .find(|o| o.role == role)
            .map(|o| o.actor_id.as_slice());
        let (options, selected) = actor_options(state, t::ROLE_ADDRESS_ADMIN_DEFAULT, current);
        els.push(
            scope(Element::select(
                format!(
                    "admin-dns-domain-role-address-{}-select",
                    role.as_storage_key()
                ),
                selected,
                SelectTarget::DnsRoleAddress { row: i, role },
                options,
            ))
            .labelled(format!("{}@", role.as_storage_key())),
        );
    }

    // The primary badge is read-only and paints on the primary row ONLY (ui.yaml
    // scopes it that way), so it is not an index-aligned column.
    if row.is_primary {
        els.push(scope(Element::label(
            ids::ADMIN_DNS_DOMAIN_PRIMARY_BADGE,
            t::PRIMARY_BADGE,
        )));
    }
    // The remove button is present on EVERY active row and merely *disabled* on
    // the primary — that is what keeps its index aligned with
    // `admin-dns-domain-name`, which the driver's `remove_domain` relies on. The
    // nest is still the authority (it refuses `cannot_remove_primary_domain`).
    els.push(scope(Element::gesture_button(
        ids::ADMIN_DNS_DOMAIN_REMOVE_BUTTON,
        t::REMOVE,
        !row.is_primary,
        Gesture::Admin(Action::RemoveDnsDomain {
            domain: row.domain.clone(),
        }),
    )));

    // The rename affordances: the primary row opens the wizard (disabled until a
    // non-primary domain exists — the two-step rule, surfaced by the shared
    // `rename_available` hint, with the nest re-validating); every non-primary
    // row can pre-target itself as the promotion target.
    if row.is_primary {
        els.push(scope(Element::gesture_button(
            ids::ADMIN_DNS_DOMAIN_RENAME_BUTTON,
            t::rename::BUTTON,
            rename_available && active_rename.is_none(),
            Gesture::Admin(Action::OpenDnsRenameSheet { target: None }),
        )));
        // During an in-flight rename the primary row states it read-only.
        if let Some(rename) = active_rename {
            els.push(scope(Element::label(
                ids::ADMIN_DNS_DOMAIN_RENAME_STATE,
                format!("{} {}", t::rename::RENAMING_TO, rename.new_primary_domain),
            )));
        }
    } else {
        els.push(scope(Element::gesture_button(
            ids::ADMIN_DNS_DOMAIN_PROMOTE_BUTTON,
            t::rename::PROMOTE,
            active_rename.is_none(),
            Gesture::Admin(Action::OpenDnsRenameSheet {
                target: Some(row.domain.clone()),
            }),
        )));
    }

    // ── The per-domain TLS-cert lifecycle ───────────────────────────────────
    els.extend(
        cert_elements(
            state,
            i,
            &row.domain,
            managed,
            cert,
            delegation,
            pending_cert,
        )
        .into_iter()
        .map(scope),
    );

    // ── This domain's record matrix ─────────────────────────────────────────
    for record in matrix.map(|d| d.records.as_slice()).unwrap_or_default() {
        els.extend(record_elements(record).into_iter().map(scope));
    }
    els
}

/// One domain's TLS-cert lifecycle affordances (`tls-certificates.md` § C.4 for
/// the badge, § B tier 2/3 for issuance + delegation). Returned **unscoped** — the
/// caller maps this domain's `.within(ids::ADMIN_DNS_DOMAIN, i)` over the whole list,
/// which is what makes the driver's `scope="admin-dns-domain[<i>]"` reads resolve
/// to exactly this row's copy of these repeated ids.
///
/// Three states, in the order the admin meets them:
///
/// 1. **The badge** — always painted, so a domain with no cert-status row yet
///    still contributes one element (index alignment) reading "Checking…".
/// 2. **Issuance** — the get/renew button branches on whether a client *can*
///    auto-publish `_acme-challenge`: managed **or** delegated → one `IssueCert`;
///    otherwise the two-phase manual paste flow. While a manual order is pending
///    *for this domain* the button is disabled (one order at a time) and the
///    paste surface renders: the challenge TXT row(s) — as plain
///    `admin-dns-record` rows, the same component every record uses, never a
///    parallel surface — plus complete/cancel.
/// 3. **Renewal delegation** — either the one-time CNAME already set (with a
///    "renewals automated" label and a remove affordance) or a reveal-on-demand
///    form to create it. Disabled with no held credential: there is no controlled
///    zone to re-home the challenge into, and `DelegateRenewal` would reject it.
fn cert_elements(
    state: &AdminState,
    i: usize,
    domain: &str,
    managed: bool,
    cert: Option<&CertStatusRow>,
    delegation: Option<&DelegationView>,
    pending_cert: Option<&PendingCertIssue>,
) -> Vec<Element> {
    let mut els = vec![Element::label(
        ids::ADMIN_DNS_CERT_STATUS,
        cert_status_text(cert),
    )];

    // A domain a client can auto-publish for finishes in one dispatch; anything
    // else needs the admin to paste the challenge.
    let single_issue = managed || delegation.is_some();
    let pending_here = pending_cert.is_some_and(|p| p.domain == domain);
    els.push(Element::gesture_button(
        ids::ADMIN_DNS_CERT_ISSUE_BUTTON,
        t::cert::ISSUE,
        !pending_here,
        Gesture::Admin(Action::IssueDnsCert {
            domain: domain.to_string(),
            single_issue,
        }),
    ));

    if let Some(pending) = pending_cert.filter(|p| p.domain == domain) {
        els.push(Element::chrome(t::cert::PASTE_INSTRUCTIONS));
        // The challenge TXT(s) reuse the `admin-dns-record` component — one record
        // type, one path (`tls-certificates.md` § The `_acme-challenge` record).
        for challenge in &pending.challenges {
            els.extend(record_elements(challenge));
        }
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_CERT_COMPLETE_BUTTON,
            t::cert::ISSUE_COMPLETE,
            true,
            Gesture::Admin(Action::CompleteDnsManualIssue),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_CERT_CANCEL_BUTTON,
            t::cert::ISSUE_CANCEL,
            true,
            Gesture::Admin(Action::CancelDnsManualIssue),
        ));
    }

    // ── Renewal delegation ──────────────────────────────────────────────────
    if let Some(d) = delegation {
        els.push(Element::chrome(t::cert::RENEWALS_AUTOMATED));
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_CERT_REMOVE_DELEGATION_BUTTON,
            t::cert::REMOVE_DELEGATION,
            true,
            Gesture::Admin(Action::RemoveDnsDelegation {
                domain: domain.to_string(),
            }),
        ));
        // The one-time CNAME the admin sets once at their registrar — again a
        // plain record row, not a bespoke widget.
        els.extend(record_elements(&d.cname));
        return els;
    }

    let zones = credential_zones(state);
    if state.dns_delegate_open.as_deref() == Some(domain) {
        els.push(
            Element::select(
                ids::ADMIN_DNS_CERT_DELEGATE_ZONE_SELECT,
                state.dns_delegate_zone.clone(),
                SelectTarget::DnsDelegateZone { row: i },
                zones.clone(),
            )
            .labelled(t::cert::DELEGATE_ZONE_LABEL),
        );
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_CERT_DELEGATE_SUBMIT_BUTTON,
            t::cert::DELEGATE_SUBMIT,
            !state.dns_delegate_zone.is_empty(),
            Gesture::Admin(Action::SubmitDnsDelegate),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_CERT_DELEGATE_CANCEL_BUTTON,
            t::cert::DELEGATE_CANCEL,
            true,
            Gesture::Admin(Action::CancelDnsDelegate),
        ));
    } else {
        // Painted-but-disabled with no held credential, never absent: an absent
        // button reads to the driver as "this app did not build the affordance".
        // The reason rides the label so the terminal needs no tooltip.
        let no_zones = zones.is_empty();
        els.push(Element::gesture_button(
            ids::ADMIN_DNS_CERT_DELEGATE_BUTTON,
            if no_zones {
                t::cert::DELEGATE_NO_ZONES
            } else {
                t::cert::DELEGATE
            },
            !no_zones,
            Gesture::Admin(Action::OpenDnsDelegate {
                domain: domain.to_string(),
            }),
        ));
    }
    els
}

/// One `admin-dns-record` row and its leaves, in ui.yaml order. Shared by the
/// steady-state matrix, the transient `_acme-challenge` paste card, and the
/// one-time delegation CNAME — all three are the same record shape, so all three
/// render through here rather than three near-copies that can drift.
fn record_elements(record: &fauna_client_dns::DnsRecordRow) -> Vec<Element> {
    let mut els = vec![
        Element::label(ids::ADMIN_DNS_RECORD, record.name.clone()),
        Element::label(ids::ADMIN_DNS_RECORD_NAME, or_dash(record.name.clone()))
            .labelled(t::FIELD_NAME),
        Element::label(
            ids::ADMIN_DNS_RECORD_TYPE,
            or_dash(record.record_type.clone()),
        )
        .labelled(t::FIELD_TYPE),
        Element::label(
            ids::ADMIN_DNS_RECORD_VALUE,
            // The exact zone-file RDATA the admin pastes. A record whose expected
            // value is somehow empty still contributes its element — an absent one
            // would shift every later record's index out of step with the name the
            // driver matched on.
            or_dash(record.expected.clone()),
        )
        .labelled(t::FIELD_VALUE),
        Element::label(ids::ADMIN_DNS_RECORD_STATUS, record_status_text(record)),
        Element::gesture_button(
            ids::ADMIN_DNS_RECORD_COPY_BUTTON,
            t::COPY,
            true,
            // The same OSC-52 clipboard path the minted-invite-code copy uses;
            // the value is also painted, so a clipboard-less terminal loses nothing.
            Gesture::Admin(Action::CopyDnsRecord {
                value: record.expected.clone(),
            }),
        ),
    ];
    // PTR can never be zone-published — reverse DNS is set at the IP owner
    // (dns-management.md § Records covered) — so only this row gets the
    // advisory instead of the normal paste-and-verify treatment.
    if record.record_type == "PTR" {
        els.push(Element::label(
            ids::ADMIN_DNS_RECORD_PROVIDER_NOTE,
            t::PTR_PROVIDER_NOTE,
        ));
    }
    els
}

/// The start-a-rename wizard sheet (`mail-primary-domain-rename.md` § UX
/// surface): pick a promotion target from the existing active non-primary
/// domains (the wizard never *adds* a domain — the two-step rule), optionally
/// override the grace window, submit. Every precondition is the nest's to
/// validate; a refusal lands on `error-message`.
fn rename_sheet_elements(
    state: &AdminState,
    domains: Option<&LocalDomainsSnapshot>,
) -> Vec<Element> {
    let targets: Vec<String> = domains
        .map(|s| {
            rename_target_names(s)
                .into_iter()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    vec![
        Element::label(ids::ADMIN_DNS_RENAME_SHEET, t::rename::SHEET_TITLE),
        Element::select(
            ids::ADMIN_DNS_RENAME_NEW_PRIMARY_SELECT,
            state.dns_rename_target.clone(),
            SelectTarget::DnsRenameTarget,
            targets.clone(),
        )
        .labelled(t::rename::NEW_PRIMARY_LABEL)
        .within(ids::ADMIN_DNS_RENAME_SHEET, 0),
        Element::input(
            ids::ADMIN_DNS_RENAME_GRACE_DAYS_INPUT,
            state.dns_rename_grace_days.clone(),
            Field::Admin(AdminField::DnsRenameGraceDays),
        )
        .labelled(t::rename::GRACE_DAYS_LABEL)
        .within(ids::ADMIN_DNS_RENAME_SHEET, 0),
        Element::gesture_button(
            ids::ADMIN_DNS_RENAME_SUBMIT_BUTTON,
            t::rename::SUBMIT,
            !targets.is_empty() && !state.dns_rename_target.is_empty(),
            Gesture::Admin(Action::SubmitDnsRename),
        )
        .within(ids::ADMIN_DNS_RENAME_SHEET, 0),
        Element::gesture_button(
            ids::ADMIN_DNS_RENAME_CANCEL_BUTTON,
            t::rename::CANCEL,
            true,
            Gesture::Admin(Action::CancelDnsRenameSheet),
        )
        .within(ids::ADMIN_DNS_RENAME_SHEET, 0),
    ]
}

/// The deployment-wide in-flight rename banner: the lifecycle actions, each
/// gated on the **shared** rename view's own affordance flag (`can_complete` /
/// `can_force_complete` / `can_extend` / `can_abort`) rather than a re-derived
/// clock read — the nest owns `now`. Complete and Abort are reveal-then-confirm
/// pairs, painted inline; their confirms name the cost (a cache-flush risk
/// before the grace window ends, an expensive inverse re-flip after the anchor
/// flipped).
fn rename_banner_elements(
    state: &AdminState,
    rename: &fauna_client_mail_settings::primary_domain_rename::PrimaryDomainRenameView,
) -> Vec<Element> {
    let scope = |el: Element| el.within(ids::ADMIN_DNS_RENAME_BANNER, 0);
    let mut els = vec![
        Element::label(ids::ADMIN_DNS_RENAME_BANNER, t::rename::BANNER_TITLE),
        Element::chrome(format!(
            "{} {} → {}",
            t::rename::STATE_LABEL,
            rename.old_primary_domain,
            rename.new_primary_domain
        )),
    ];
    if rename.can_complete || rename.can_force_complete {
        els.push(scope(Element::gesture_button(
            ids::ADMIN_DNS_RENAME_COMPLETE_BUTTON,
            t::rename::COMPLETE,
            true,
            Gesture::Admin(Action::OpenDnsRenameCompleteConfirm),
        )));
        if state.dns_rename_completing {
            // Forcing before the grace window elapses can briefly break delivery
            // for peers whose caches have not refreshed — the confirm says so.
            if rename.can_force_complete {
                els.push(Element::chrome(t::rename::COMPLETE_FORCE_WARNING));
            }
            els.push(scope(Element::gesture_button(
                ids::ADMIN_DNS_RENAME_COMPLETE_CONFIRM_BUTTON,
                t::rename::COMPLETE_CONFIRM,
                true,
                Gesture::Admin(Action::ConfirmDnsRenameComplete),
            )));
        }
    }
    if rename.can_extend {
        els.push(scope(
            Element::input(
                ids::ADMIN_DNS_RENAME_EXTEND_DAYS_INPUT,
                state.dns_rename_extend_days.clone(),
                Field::Admin(AdminField::DnsRenameExtendDays),
            )
            .labelled(t::rename::EXTEND_DAYS_LABEL),
        ));
        els.push(scope(Element::gesture_button(
            ids::ADMIN_DNS_RENAME_EXTEND_BUTTON,
            t::rename::EXTEND,
            true,
            Gesture::Admin(Action::ExtendDnsRenameGrace),
        )));
    }
    if rename.can_abort {
        els.push(scope(Element::gesture_button(
            ids::ADMIN_DNS_RENAME_ABORT_BUTTON,
            t::rename::ABORT,
            true,
            Gesture::Admin(Action::OpenDnsRenameAbortConfirm),
        )));
        if state.dns_rename_aborting {
            if rename.is_post_flip_active {
                els.push(Element::chrome(t::rename::ABORT_POSTFLIP_WARNING));
            }
            els.push(scope(Element::gesture_button(
                ids::ADMIN_DNS_RENAME_ABORT_CONFIRM_BUTTON,
                t::rename::ABORT_CONFIRM,
                true,
                Gesture::Admin(Action::ConfirmDnsRenameAbort),
            )));
        }
    }
    els
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::admin::AdminPage;
    use fauna_client_dns::{
        CertHealthState, CredentialSummary, DnsRecordRow, DnsStatus, RecordVerdict, VerifyStatus,
    };
    use fauna_client_mail_settings::local_domains::{LocalDomainStatus, RoleAddressOverrideView};
    use fauna_client_mail_settings::primary_domain_rename::PrimaryDomainRenameView;

    fn record(name: &str, kind: &str, value: &str) -> DnsRecordRow {
        DnsRecordRow {
            name: name.to_string(),
            record_type: kind.to_string(),
            expected: value.to_string(),
            ttl_seconds: 300,
            verdict: None,
        }
    }

    fn matrix(domain: &str, records: Vec<DnsRecordRow>) -> DomainView {
        DomainView {
            domain: domain.to_string(),
            mode: fauna_client_dns::MODE_MANUAL.to_string(),
            is_primary: false,
            records,
            auto_renew: false,
        }
    }

    fn dns_snapshot(domains: Vec<DomainView>) -> fauna_client_dns::DnsSnapshot {
        fauna_client_dns::DnsSnapshot {
            domains,
            credentials: Vec::new(),
            status: DnsStatus::Idle,
            error: None,
            pending_cert: None,
            delegations: Vec::new(),
            cert_statuses: Vec::new(),
        }
    }

    /// Every record-status spelling this page can paint comes from the shared
    /// `dns_verdict_label`, resolved through tui's own table — never a local
    /// re-derivation of the verdict → key decision.
    ///
    /// The mapping is byte-identical to the hand-rolled `match` this replaced, so
    /// no behavioural test could have caught the duplication and none did (before
    /// this, `record_status_text` had no direct coverage at all — which is how it
    /// stayed the last un-adopted leg on a page whose sibling `cert_status_text`
    /// had already consumed its shared view). What this pins instead is the
    /// *equivalence*: re-hand-roll the match and drift any arm, and this reds.
    /// That is the same "pin the seam a behavioural test cannot see" shape the
    /// per-target `cfg` lesson left behind.
    #[test]
    fn every_record_status_spelling_comes_from_the_shared_verdict_label() {
        let shared = |variant: &str, observed: &[String]| {
            fauna_core::format::dns_verdict_label(variant, observed)
                .resolve(fauna_i18n::strings::lookup)
        };
        let with = |status: VerifyStatus, observed: &[&str]| {
            let mut row = record("a.example.com", "A", "1.2.3.4");
            row.verdict = Some(RecordVerdict {
                observed: observed.iter().map(|s| s.to_string()).collect(),
                status,
            });
            record_status_text(&row)
        };

        for (status, variant) in [
            (VerifyStatus::Ok, "Ok"),
            (VerifyStatus::Missing, "Missing"),
            (VerifyStatus::Checking, "Checking"),
        ] {
            assert_eq!(with(status, &[]), shared(variant, &[]), "{variant}");
        }

        // A `Mismatch` names what public DNS actually served — the arm that
        // carries an arg, and the only one whose text varies with the data.
        let observed = ["9.9.9.9".to_string(), "8.8.8.8".to_string()];
        assert_eq!(
            with(VerifyStatus::Mismatch, &["9.9.9.9", "8.8.8.8"]),
            shared("Mismatch", &observed)
        );
        assert!(
            with(VerifyStatus::Mismatch, &["9.9.9.9"]).contains("9.9.9.9"),
            "a red verdict must say mismatch with WHAT"
        );
        // A mismatch with no observed values (only reachable from a nest too old
        // to send them) degrades to the plain label rather than an empty arg.
        assert_eq!(
            with(VerifyStatus::Mismatch, &[]),
            shared("Mismatch", &[]),
            "empty observed degrades, it does not render a blank found-list"
        );

        // An absent verdict is the neutral no-verdict state, never a false
        // green/red — it must read exactly as `Checking`, not as its own spelling.
        let pending = record("a.example.com", "A", "1.2.3.4");
        assert_eq!(pending.verdict, None, "the helper's baseline is unverified");
        assert_eq!(record_status_text(&pending), shared("Checking", &[]));
    }

    pub(crate) fn domain_row(name: &str, primary: bool) -> LocalDomainView {
        LocalDomainView {
            domain_id: name.as_bytes().to_vec(),
            domain: name.to_string(),
            is_primary: primary,
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
            dmarc_policy: fauna_client_mail_settings::DomainDmarcPolicy::Reject,
            added_at: 0,
            removed_at: None,
        }
    }

    pub(crate) fn domains_snapshot(
        active: Vec<LocalDomainView>,
        soft_deleted: Vec<LocalDomainView>,
    ) -> LocalDomainsSnapshot {
        let adding_first_domain = active.is_empty();
        LocalDomainsSnapshot {
            active,
            soft_deleted,
            status: LocalDomainStatus::Idle,
            error: None,
            last_add_skipped: false,
            active_rename: None,
            rename_available: false,
            adding_first_domain,
        }
    }

    fn app_with(
        dns: Option<fauna_client_dns::DnsSnapshot>,
        domains: Option<LocalDomainsSnapshot>,
    ) -> crate::app::App {
        // Authenticated and parked on Admin on purpose: `App::error_line_text`
        // reports the *launch* surface's error while signed out, so a signed-out
        // fixture cannot see a page error at all.
        let mut app = crate::app::tests::authed_app();
        app.page = crate::pages::Page::Admin;
        app.admin.sub = AdminPage::Dns;
        app.admin.dns_snapshot = dns;
        app.admin.local_domains_snapshot = domains;
        app
    }

    fn texts<'a>(els: &'a [Element], id: &str) -> Vec<&'a str> {
        els.iter()
            .filter(|e| e.id == id)
            .map(|e| e.text.as_str())
            .collect()
    }

    /// Whether the (single) checkbox with this id is checked.
    fn is_checked(els: &[Element], id: &str) -> bool {
        els.iter().any(|e| {
            e.id == id && matches!(e.role, crate::element::Role::Checkbox { checked: true, .. })
        })
    }

    /// The option list of the (single) picker with this id.
    fn options_of(els: &[Element], id: &str) -> Vec<String> {
        els.iter()
            .find(|e| e.id == id)
            .and_then(|e| match &e.role {
                crate::element::Role::Select { options, .. } => Some(options.clone()),
                _ => None,
            })
            .expect("a picker with options")
    }

    /// The page's own ui.yaml landmarks paint even on a fresh nest with no
    /// domains, so the driver's nav can wait on something real (and the
    /// credentials list is what proves the page is wired to the credential-store
    /// machine — `test_admin_dns_credentials_list_and_refresh` asserts exactly
    /// that, and it must be visible while EMPTY).
    #[test]
    fn page_landmarks_and_empty_credentials_list_render_with_no_data() {
        let app = app_with(None, None);
        let els = dns_elements(&app.admin);
        for id in [
            "page-heading",
            "admin-nav-back",
            "admin-dns-refresh-button",
            "admin-dns-manage-all-toggle",
            "admin-dns-credentials-list",
            "admin-dns-add-domain-button",
            "admin-dns-add-credential-button",
        ] {
            assert_eq!(texts(&els, id).len(), 1, "{id} must paint exactly once");
        }
        assert!(texts(&els, "admin-dns-credential-item").is_empty());
        assert!(texts(&els, "error-message").is_empty());
    }

    /// A domain's records render with their exact expected values — the page's
    /// core contract (`test_admin_dns_lists_domain_records`). The rows come from
    /// the LOCAL-DOMAIN snapshot and the records are looked up by name from the
    /// DNS snapshot, so a row with no matrix entry yet still paints (with no
    /// records) rather than vanishing.
    #[test]
    fn domain_rows_come_from_local_domains_and_records_are_looked_up_by_name() {
        let app = app_with(
            Some(dns_snapshot(vec![matrix(
                "a.test",
                vec![
                    record("a.test", "MX", "10 mail.a.test."),
                    record("_dmarc.a.test", "TXT", "v=DMARC1; p=reject"),
                ],
            )])),
            Some(domains_snapshot(
                vec![domain_row("a.test", true), domain_row("b.test", false)],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-dns-domain-name"),
            vec!["a.test", "b.test"],
            "a row with no matrix entry must still paint"
        );
        assert_eq!(
            texts(&els, "admin-dns-record-name"),
            vec!["a.test", "_dmarc.a.test"]
        );
        assert_eq!(
            texts(&els, "admin-dns-record-value"),
            vec!["10 mail.a.test.", "v=DMARC1; p=reject"]
        );
        assert_eq!(texts(&els, "admin-dns-record-type"), vec!["MX", "TXT"]);
    }

    /// Every active row contributes exactly one element per per-row id — the
    /// remove button included, which is *present but disabled* on the primary so
    /// its index stays aligned with `admin-dns-domain-name` (the contract
    /// `actions/admin.py::remove_domain` relies on when it resolves a row by name
    /// and then clicks the button at that same index).
    #[test]
    fn per_row_indexes_line_up_across_every_domain_id() {
        let app = app_with(
            None,
            Some(domains_snapshot(
                vec![
                    domain_row("primary.test", true),
                    domain_row("second.test", false),
                    domain_row("third.test", false),
                ],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        for id in [
            "admin-dns-domain",
            "admin-dns-domain-name",
            "admin-dns-domain-mode",
            "admin-dns-domain-remove-button",
            "admin-dns-domain-catch-all-select",
            "admin-dns-domain-role-address-postmaster-select",
            "admin-dns-domain-role-address-abuse-select",
            "admin-dns-domain-role-address-noc-select",
            "admin-dns-domain-role-address-security-select",
        ] {
            assert_eq!(texts(&els, id).len(), 3, "{id} must paint once per row");
        }
        // Present on every row, disabled only on the primary.
        let remove: Vec<bool> = els
            .iter()
            .filter(|e| e.id == "admin-dns-domain-remove-button")
            .map(|e| e.enabled)
            .collect();
        assert_eq!(remove, vec![false, true, true]);
        // The badge is primary-only (ui.yaml scopes it that way), so it is NOT an
        // index-aligned column.
        assert_eq!(texts(&els, "admin-dns-domain-primary-badge").len(), 1);
    }

    /// A record with **no verdict yet** still contributes its status element — an
    /// empty label would not register and would drop that record from the id's
    /// index sequence, mis-addressing every later row. Reads "Checking…", never a
    /// false green or red.
    #[test]
    fn unverified_records_still_contribute_one_status_element_per_row() {
        let mut verified = record("a.test", "MX", "10 mail.a.test.");
        verified.verdict = Some(RecordVerdict {
            observed: Vec::new(),
            status: VerifyStatus::Ok,
        });
        let mut mismatched = record("_dmarc.a.test", "TXT", "v=DMARC1; p=reject");
        mismatched.verdict = Some(RecordVerdict {
            observed: vec!["v=DMARC1; p=none".to_string()],
            status: VerifyStatus::Mismatch,
        });
        let app = app_with(
            Some(dns_snapshot(vec![matrix(
                "a.test",
                vec![
                    verified,
                    mismatched,
                    record("_mta-sts.a.test", "TXT", "v=STSv1; id=1"),
                ],
            )])),
            Some(domains_snapshot(
                vec![domain_row("a.test", true)],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        let statuses = texts(&els, "admin-dns-record-status");
        assert_eq!(statuses.len(), 3, "one status per record, verdict or not");
        assert_eq!(statuses[0], t::STATUS_OK);
        assert!(
            statuses[1].contains("v=DMARC1; p=none"),
            "a mismatch names what public DNS actually serves: {:?}",
            statuses[1]
        );
        assert_eq!(statuses[2], t::STATUS_CHECKING);
    }

    /// The mode control's TEXT is the mode label the driver reads back, and
    /// managed-ness is the SHARED `DomainView::is_managed` projection — a domain
    /// with no matrix row reads manual rather than guessing.
    #[test]
    fn mode_control_text_is_the_shared_mode_projection() {
        let mut managed = matrix("managed.test", Vec::new());
        managed.mode = fauna_client_dns::MODE_MANAGED.to_string();
        let app = app_with(
            Some(dns_snapshot(vec![
                managed,
                matrix("manual.test", Vec::new()),
            ])),
            Some(domains_snapshot(
                vec![
                    domain_row("managed.test", true),
                    domain_row("manual.test", false),
                    domain_row("unknown.test", false),
                ],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-dns-domain-mode"),
            vec![t::MODE_MANAGED, t::MODE_MANUAL, t::MODE_MANUAL]
        );
    }

    /// The master switch reflects the shared `all_domains_managed` fold over the
    /// ACTIVE rows — so an active domain the matrix has no row for keeps it off
    /// (never a false "everything is managed").
    #[test]
    fn manage_all_toggle_is_the_shared_fold_over_active_rows() {
        let managed = |name: &str| {
            let mut d = matrix(name, Vec::new());
            d.mode = fauna_client_dns::MODE_MANAGED.to_string();
            d
        };
        let app = app_with(
            Some(dns_snapshot(vec![managed("a.test"), managed("b.test")])),
            Some(domains_snapshot(
                vec![domain_row("a.test", true), domain_row("b.test", false)],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        assert!(is_checked(&els, "admin-dns-manage-all-toggle"));

        // A third active domain with no managed matrix row flips it off.
        let app = app_with(
            Some(dns_snapshot(vec![managed("a.test"), managed("b.test")])),
            Some(domains_snapshot(
                vec![
                    domain_row("a.test", true),
                    domain_row("b.test", false),
                    domain_row("c.test", false),
                ],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        assert!(!is_checked(&els, "admin-dns-manage-all-toggle"));
    }

    /// Soft-deleted domains render as their own id family with a Restore
    /// affordance, and never as active rows — so a removed domain is unambiguous
    /// to the driver (`test_admin_dns_add_remove_restore_domain`).
    #[test]
    fn soft_deleted_domains_render_their_own_id_family() {
        let app = app_with(
            None,
            Some(domains_snapshot(
                vec![domain_row("live.test", true)],
                vec![domain_row("gone.test", false)],
            )),
        );
        let els = dns_elements(&app.admin);
        assert_eq!(texts(&els, "admin-dns-domain-name"), vec!["live.test"]);
        assert_eq!(
            texts(&els, "admin-dns-removed-domain-name"),
            vec!["gone.test"]
        );
        assert_eq!(
            texts(&els, "admin-dns-removed-domain-restore-button").len(),
            1
        );
    }

    /// The add-domain and add-credential forms are **inline reveals**: nothing of
    /// either paints until its reveal button is clicked, so `is_visible` on a form
    /// element is false beforehand and true in the very same frame after.
    #[test]
    fn forms_are_absent_until_revealed() {
        let app = app_with(None, None);
        let els = dns_elements(&app.admin);
        for id in [
            "admin-dns-add-domain-input",
            "admin-dns-add-domain-submit-button",
            "admin-dns-add-credential-provider-row",
            "admin-dns-add-credential-form",
            "admin-dns-add-credential-submit-button",
        ] {
            assert!(
                !els.iter().any(|e| e.id == id),
                "{id} must not paint before its form is revealed"
            );
        }

        let mut app = app_with(None, None);
        app.admin.dns_add_domain_open = true;
        app.admin.dns_add_credential_open = true;
        let els = dns_elements(&app.admin);
        for id in [
            "admin-dns-add-domain-input",
            "admin-dns-add-domain-submit-button",
            "admin-dns-add-domain-cancel-button",
            "admin-dns-add-credential-provider-row",
            "admin-dns-add-credential-form",
            "admin-dns-add-credential-submit-button",
            "admin-dns-add-credential-cancel-button",
        ] {
            assert_eq!(
                texts(&els, id).len(),
                1,
                "{id} must paint once when revealed"
            );
        }
        // The reveal buttons themselves give way to their forms.
        assert!(!els.iter().any(|e| e.id == "admin-dns-add-domain-button"));
        assert!(
            !els.iter()
                .any(|e| e.id == "admin-dns-add-credential-button")
        );
    }

    /// A domainless nest's first add is a one-way door (`deployment-home-with-
    /// public-relay.md` § MUA reach): the primary it becomes can never be
    /// removed from any app. The warning is untagged chrome, not a ui.yaml id
    /// (`Element::chrome` — no rule-A approval needed), and only appears when
    /// `adding_first_domain` says the next add really would be the first —
    /// never once a primary already exists.
    #[test]
    fn first_domain_add_warns_of_irreversibility_only_when_domainless() {
        let mut domainless = app_with(None, Some(domains_snapshot(vec![], vec![])));
        domainless.admin.dns_add_domain_open = true;
        let els = dns_elements(&domainless.admin);
        assert!(
            els.iter()
                .any(|e| e.id.is_empty() && e.text == t::ADD_DOMAIN_PRIMARY_WARNING),
            "a domainless nest's add-domain form must warn before the submit \
             that makes the add irreversible"
        );

        let mut already_has_one = app_with(
            None,
            Some(domains_snapshot(
                vec![domain_row("existing.example", true)],
                vec![],
            )),
        );
        already_has_one.admin.dns_add_domain_open = true;
        let els = dns_elements(&already_has_one.admin);
        assert!(
            !els.iter()
                .any(|e| e.id.is_empty() && e.text == t::ADD_DOMAIN_PRIMARY_WARNING),
            "a second-or-later add is ordinary and must not warn"
        );
    }

    /// The provider buttons are keyed `[<provider_id>]` (a literal bracket in the
    /// id — the cross-app shape the driver clicks by name), and the per-field
    /// entries appear only once a provider is picked, each tagged with the RAW
    /// providers.yaml field id.
    #[test]
    fn credential_form_keys_providers_and_rebuilds_fields_on_select() {
        let mut app = app_with(None, None);
        app.admin.dns_add_credential_open = true;
        let els = dns_elements(&app.admin);
        assert!(
            els.iter()
                .any(|e| e.id == "admin-dns-add-credential-provider-row[cloudflare]"),
            "the keyed per-provider button must exist"
        );
        // No provider picked ⇒ no field entries at all.
        let field_ids = credential_field_ids("cloudflare");
        assert!(!field_ids.is_empty(), "cloudflare must expose DNS fields");
        for id in &field_ids {
            assert!(
                !els.iter().any(|e| &e.id == id),
                "{id} must not paint before a provider is picked"
            );
        }

        app.admin.dns_credential_provider = Some("cloudflare".to_string());
        let els = dns_elements(&app.admin);
        for id in &field_ids {
            assert_eq!(
                texts(&els, id).len(),
                1,
                "{id} must paint once for the picked provider"
            );
        }
        assert!(is_checked(
            &els,
            "admin-dns-add-credential-provider-row[cloudflare]"
        ));
    }

    /// Held credentials render provider + covered zones + a clear affordance,
    /// index-aligned, and never the secret (the form is write-only).
    #[test]
    fn held_credentials_render_provider_zones_and_clear() {
        let mut snap = dns_snapshot(Vec::new());
        snap.credentials = vec![
            CredentialSummary {
                provider_id: "cloudflare".to_string(),
                zones: vec!["a.test".to_string(), "b.test".to_string()],
                label: "cloudflare".to_string(),
            },
            CredentialSummary {
                provider_id: "hetzner".to_string(),
                // A zero-zone row still contributes its zones element, or every
                // later row's index shifts under the driver.
                zones: Vec::new(),
                label: "hetzner".to_string(),
            },
        ];
        let app = app_with(Some(snap), None);
        let els = dns_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-dns-credential-item-provider"),
            vec!["cloudflare", "hetzner"]
        );
        assert_eq!(texts(&els, "admin-dns-credential-item-zones").len(), 2);
        assert_eq!(
            texts(&els, "admin-dns-credential-item-clear-button").len(),
            2
        );
    }

    /// The rename sheet is an inline reveal whose picker offers the active
    /// NON-primary domains (the two-step rule), and the primary row's rename
    /// button is disabled until one exists (`rename_available`).
    #[test]
    fn rename_sheet_reveals_and_offers_non_primary_targets() {
        let mut domains = domains_snapshot(
            vec![
                domain_row("primary.test", true),
                domain_row("second.test", false),
            ],
            Vec::new(),
        );
        domains.rename_available = true;
        let mut app = app_with(None, Some(domains));
        let els = dns_elements(&app.admin);
        assert!(
            !els.iter().any(|e| e.id == "admin-dns-rename-sheet"),
            "the sheet must not paint until opened"
        );
        assert!(
            els.iter()
                .any(|e| e.id == "admin-dns-domain-rename-button" && e.enabled),
            "a non-primary domain exists, so Rename primary enables"
        );
        assert_eq!(texts(&els, "admin-dns-domain-promote-button").len(), 1);

        app.admin.dns_rename_sheet_open = true;
        app.admin.dns_rename_target = "second.test".to_string();
        let els = dns_elements(&app.admin);
        assert_eq!(texts(&els, "admin-dns-rename-sheet").len(), 1);
        assert_eq!(
            texts(&els, "admin-dns-rename-new-primary-select"),
            vec!["second.test"]
        );
        assert_eq!(
            options_of(&els, "admin-dns-rename-new-primary-select"),
            vec!["second.test".to_string()]
        );
        assert!(
            els.iter()
                .any(|e| e.id == "admin-dns-rename-submit-button" && e.enabled)
        );
        assert_eq!(texts(&els, "admin-dns-rename-cancel-button").len(), 1);
        // Opening the sheet must not conjure the in-flight banner.
        assert!(!els.iter().any(|e| e.id == "admin-dns-rename-banner"));
    }

    /// With no non-primary domain the two-step rule blocks the wizard, so the
    /// primary row's rename button paints **disabled** rather than absent (a
    /// missing button reads to the driver as "this client didn't build it").
    #[test]
    fn rename_button_is_disabled_without_a_promotion_target() {
        let app = app_with(
            None,
            Some(domains_snapshot(
                vec![domain_row("only.test", true)],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        assert!(
            els.iter()
                .any(|e| e.id == "admin-dns-domain-rename-button" && !e.enabled)
        );
        assert!(texts(&els, "admin-dns-domain-promote-button").is_empty());
    }

    /// The banner's lifecycle actions are gated on the SHARED rename view's own
    /// affordance flags, and Complete/Abort are reveal-then-confirm pairs — the
    /// confirm is absent until armed (the rotate-confirm shape).
    #[test]
    fn rename_banner_gates_actions_on_the_shared_affordance_flags() {
        let rename = PrimaryDomainRenameView {
            rename_id: vec![1, 2, 3],
            state: "pre_flip".to_string(),
            old_primary_domain: "old.test".to_string(),
            new_primary_domain: "new.test".to_string(),
            started_at: 0,
            grace_days: 7,
            grace_ends_at: None,
            ready_to_complete_at: None,
            is_post_flip_active: false,
            is_pre_flip: true,
            can_complete: true,
            can_force_complete: false,
            can_extend: false,
            can_abort: true,
        };
        let mut domains = domains_snapshot(
            vec![domain_row("old.test", true), domain_row("new.test", false)],
            Vec::new(),
        );
        domains.active_rename = Some(rename);
        let mut app = app_with(None, Some(domains));
        let els = dns_elements(&app.admin);
        assert_eq!(texts(&els, "admin-dns-rename-banner").len(), 1);
        assert_eq!(texts(&els, "admin-dns-rename-complete-button").len(), 1);
        assert_eq!(texts(&els, "admin-dns-rename-abort-button").len(), 1);
        // `can_extend: false` ⇒ no extend affordance at all.
        assert!(texts(&els, "admin-dns-rename-extend-button").is_empty());
        assert!(texts(&els, "admin-dns-rename-extend-days-input").is_empty());
        // Neither confirm is painted until armed.
        assert!(texts(&els, "admin-dns-rename-complete-confirm-button").is_empty());
        assert!(texts(&els, "admin-dns-rename-abort-confirm-button").is_empty());
        // The primary row states the in-flight rename read-only, and neither
        // rename entry point re-arms while one is running.
        assert!(
            texts(&els, "admin-dns-domain-rename-state")
                .first()
                .is_some_and(|s| s.contains("new.test"))
        );
        assert!(
            els.iter()
                .any(|e| e.id == "admin-dns-domain-rename-button" && !e.enabled)
        );

        app.admin.dns_rename_completing = true;
        app.admin.dns_rename_aborting = true;
        let els = dns_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-dns-rename-complete-confirm-button").len(),
            1
        );
        assert_eq!(
            texts(&els, "admin-dns-rename-abort-confirm-button").len(),
            1
        );
    }

    /// A per-domain designation renders as the picker's SELECTED label, and a
    /// designation the actor list doesn't carry keeps a trailing fallback option
    /// so a stale pick can't read as an accidental clear.
    #[test]
    fn actor_pickers_render_designations_and_keep_unknown_ones_visible() {
        let mut row = domain_row("a.test", true);
        row.catch_all_actor_id = Some(vec![0xaa; 32]);
        row.role_address_overrides = vec![RoleAddressOverrideView {
            role: RoleAddressKind::Abuse,
            actor_id: vec![0xbb; 32],
        }];
        let mut app = app_with(None, Some(domains_snapshot(vec![row], Vec::new())));
        app.admin.dns_actors = vec![(vec![0xaa; 32], "Alice".to_string())];
        let els = dns_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-dns-domain-catch-all-select"),
            vec!["Alice"]
        );
        // 0xbb is not in the actor list — it stays visible via the fallback,
        // as the FULL 32-byte hex (widened from a 4-byte-truncated
        // prefix to match apple/linux's `hex_full`), not just a prefix match.
        let abuse = texts(&els, "admin-dns-domain-role-address-abuse-select");
        assert_eq!(abuse, vec![format!("actor {}…", "bb".repeat(32))]);
        // An un-overridden role reads the clear sentinel.
        assert_eq!(
            texts(&els, "admin-dns-domain-role-address-noc-select"),
            vec![t::ROLE_ADDRESS_ADMIN_DEFAULT]
        );
    }

    /// The succession-cleared explainer renders ONLY when the nest says a
    /// succession (not an admin) last cleared this domain's catch-all — the
    /// distinct state row 242 exists to surface, since the picker alone reads
    /// identically ("None") whether the catch-all was never set or was
    /// cleared out from under the admin.
    #[test]
    fn catch_all_cleared_by_succession_state_renders_only_when_flagged() {
        let mut row = domain_row("a.test", true);
        row.catch_all_cleared_by_succession_at = Some(1_700_000_000_000);
        let app = app_with(None, Some(domains_snapshot(vec![row], Vec::new())));
        let els = dns_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-dns-domain-catch-all-cleared-state"),
            vec![t::CATCH_ALL_CLEARED_BY_SUCCESSION]
        );

        // An ordinary "never set" row (the default `domain_row` fixture) must
        // NOT render the explainer — that would misreport an untouched
        // domain as succession-cleared.
        let untouched = domain_row("b.test", false);
        let app = app_with(None, Some(domains_snapshot(vec![untouched], Vec::new())));
        let els = dns_elements(&app.admin);
        assert!(texts(&els, "admin-dns-domain-catch-all-cleared-state").is_empty());
    }

    /// A domain-CRUD error takes precedence over a list/verify error (it is the
    /// more recent, more specific act — linux's `render_admin_dns` ordering), and
    /// either reaches the page's `error-message` (rule 2).
    #[test]
    fn errors_reach_error_message_crud_first() {
        let mut dns = dns_snapshot(Vec::new());
        dns.error = Some("list_records failed".to_string());
        let mut domains = domains_snapshot(Vec::new(), Vec::new());
        domains.error = Some("cannot_remove_primary_domain".to_string());
        let app = app_with(Some(dns.clone()), Some(domains));
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("cannot_remove_primary_domain")
        );

        // With no CRUD error the read failure surfaces instead.
        let app = app_with(Some(dns), Some(domains_snapshot(Vec::new(), Vec::new())));
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("list_records failed")
        );
        assert!(
            !dns_elements(&app.admin)
                .iter()
                .any(|e| e.id == "error-message"),
            "the id is registered by the ONE global funnel, never by the page"
        );
    }

    /// Every per-domain child is scoped to its own `admin-dns-domain[i]`, so a
    /// scoped query resolves to exactly that domain's copy of a repeated id —
    /// what `actions/admin.py`'s `scope="admin-dns-domain[<i>]"` reads rely on.
    #[test]
    fn per_domain_children_carry_their_row_scope() {
        let app = app_with(
            Some(dns_snapshot(vec![matrix(
                "b.test",
                vec![record("b.test", "MX", "10 mail.b.test.")],
            )])),
            Some(domains_snapshot(
                vec![domain_row("a.test", true), domain_row("b.test", false)],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        // The container anchor itself is unscoped (it IS the scope).
        for el in els.iter().filter(|e| e.id == "admin-dns-domain") {
            assert!(el.path.is_empty(), "the row anchor must not scope itself");
        }
        let scoped = |id: &str| -> Vec<Vec<(String, usize)>> {
            els.iter()
                .filter(|e| e.id == id)
                .map(|e| e.path.clone())
                .collect()
        };
        assert_eq!(
            scoped("admin-dns-domain-name"),
            vec![
                vec![("admin-dns-domain".to_string(), 0)],
                vec![("admin-dns-domain".to_string(), 1)],
            ]
        );
        // b.test is row 1, so its records scope there — not to row 0.
        assert_eq!(
            scoped("admin-dns-record-value"),
            vec![vec![("admin-dns-domain".to_string(), 1)]]
        );
    }
    /// The cert badge always paints (index alignment), reads "Checking…" before
    /// the status read returns, and withholds a FLOOR cert's own far-future
    /// expiry — the shared `cert_status_view` decision, not a local `else if`
    /// chain. "expires 2035" next to "renew needed" would read as reassurance.
    #[test]
    fn cert_badge_uses_the_shared_decision_and_withholds_a_floor_expiry() {
        // No cert-status row yet.
        let app = app_with(
            None,
            Some(domains_snapshot(
                vec![domain_row("a.test", true)],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        let badges = texts(&els, "admin-dns-cert-status");
        assert_eq!(badges.len(), 1, "the badge paints even with no status row");
        assert!(badges[0].contains(t::STATUS_CHECKING));

        // On the self-signed floor: the self-signed sub-label, NO expiry.
        let mut snap = dns_snapshot(vec![matrix("a.test", Vec::new())]);
        snap.cert_statuses = vec![CertStatusRow {
            domain: "a.test".to_string(),
            state: CertHealthState::OnFloorRenewNeeded,
            not_after_unix: 4_000_000_000,
            is_floor: true,
        }];
        let app = app_with(
            Some(snap),
            Some(domains_snapshot(
                vec![domain_row("a.test", true)],
                Vec::new(),
            )),
        );
        let badge = texts(&dns_elements(&app.admin), "admin-dns-cert-status")[0].to_string();
        assert!(badge.contains(t::cert::SELF_SIGNED), "got {badge:?}");
        assert!(
            !badge.contains("2096") && !badge.contains("2097"),
            "a floor cert's own expiry must be withheld: {badge:?}"
        );

        // A trusted cert shows its expiry instead.
        let mut snap = dns_snapshot(vec![matrix("a.test", Vec::new())]);
        snap.cert_statuses = vec![CertStatusRow {
            domain: "a.test".to_string(),
            state: CertHealthState::ValidTrusted,
            not_after_unix: 1_767_225_600, // 2026-01-01
            is_floor: false,
        }];
        let app = app_with(
            Some(snap),
            Some(domains_snapshot(
                vec![domain_row("a.test", true)],
                Vec::new(),
            )),
        );
        let badge = texts(&dns_elements(&app.admin), "admin-dns-cert-status")[0].to_string();
        assert!(
            badge.contains("2025") || badge.contains("2026"),
            "got {badge:?}"
        );
        assert!(!badge.contains(t::cert::SELF_SIGNED));
    }

    /// Auto-renew is shown ONLY for a managed or delegated domain — the only kinds
    /// that can auto-issue — and its checked state rides the `state` attr because
    /// the label is the constant "Auto-renew" (what `auto_renew_state()` reads).
    #[test]
    fn auto_renew_shows_only_for_managed_or_delegated_domains() {
        let mut managed = matrix("managed.test", Vec::new());
        managed.mode = fauna_client_dns::MODE_MANAGED.to_string();
        managed.auto_renew = true;
        let mut delegated = matrix("delegated.test", Vec::new());
        delegated.auto_renew = false;
        let mut snap = dns_snapshot(vec![managed, delegated, matrix("manual.test", Vec::new())]);
        snap.delegations = vec![DelegationView {
            domain: "delegated.test".to_string(),
            cname: record("_acme-challenge.delegated.test", "CNAME", "target.example."),
        }];
        let app = app_with(
            Some(snap),
            Some(domains_snapshot(
                vec![
                    domain_row("managed.test", true),
                    domain_row("delegated.test", false),
                    domain_row("manual.test", false),
                ],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        let toggles: Vec<(&str, Option<&str>)> = els
            .iter()
            .filter(|e| e.id == "admin-dns-domain-auto-renew")
            .map(|e| {
                (
                    e.path
                        .first()
                        .map(|(_, i)| *i)
                        .map(|i| if i == 0 { "row0" } else { "row1" })
                        .unwrap_or("unscoped"),
                    e.attrs
                        .iter()
                        .find(|(k, _)| k == "state")
                        .map(|(_, v)| v.as_str()),
                )
            })
            .collect();
        assert_eq!(
            toggles,
            vec![("row0", Some("on")), ("row1", Some("off"))],
            "only the managed (row 0) and delegated (row 1) rows carry the toggle, \
             with their effective state on the `state` attr"
        );
    }

    /// The issue button branches on whether a client can auto-publish
    /// `_acme-challenge`: managed or delegated → one `IssueCert`; otherwise the
    /// two-phase manual paste flow. Painted on EVERY row so its index stays
    /// aligned with `admin-dns-domain-name`.
    #[test]
    fn issue_button_paths_split_on_managed_or_delegated() {
        let mut managed = matrix("managed.test", Vec::new());
        managed.mode = fauna_client_dns::MODE_MANAGED.to_string();
        let mut snap = dns_snapshot(vec![managed, matrix("manual.test", Vec::new())]);
        snap.delegations = vec![DelegationView {
            domain: "delegated.test".to_string(),
            cname: record("_acme-challenge.delegated.test", "CNAME", "target.example."),
        }];
        let app = app_with(
            Some(snap),
            Some(domains_snapshot(
                vec![
                    domain_row("managed.test", true),
                    domain_row("manual.test", false),
                    domain_row("delegated.test", false),
                ],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        let paths: Vec<bool> = els
            .iter()
            .filter(|e| e.id == "admin-dns-cert-issue-button")
            .map(|e| match &e.role {
                crate::element::Role::Button(Gesture::Admin(Action::IssueDnsCert {
                    single_issue,
                    ..
                })) => *single_issue,
                other => panic!("unexpected role {other:?}"),
            })
            .collect();
        assert_eq!(
            paths,
            vec![true, false, true],
            "managed and delegated issue in one dispatch; manual goes two-phase"
        );
    }

    /// A pending manual issuance surfaces the paste card ONLY on its own domain's
    /// section, renders the challenge TXT through the shared `admin-dns-record`
    /// component (one record type, one path), and disables that row's issue button
    /// (one order at a time).
    #[test]
    fn a_pending_manual_issuance_renders_its_paste_card_on_its_own_row_only() {
        let mut snap = dns_snapshot(vec![
            matrix("a.test", vec![record("a.test", "MX", "10 mail.a.test.")]),
            matrix("b.test", Vec::new()),
        ]);
        snap.pending_cert = Some(PendingCertIssue {
            domain: "b.test".to_string(),
            challenges: vec![record(
                "_acme-challenge.b.test",
                "TXT",
                "tokenvalue-deadbeef",
            )],
        });
        let app = app_with(
            Some(snap),
            Some(domains_snapshot(
                vec![domain_row("a.test", true), domain_row("b.test", false)],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        // The complete/cancel pair exists once, scoped to b.test (row 1).
        for id in [
            "admin-dns-cert-complete-button",
            "admin-dns-cert-cancel-button",
        ] {
            let scopes: Vec<Vec<(String, usize)>> = els
                .iter()
                .filter(|e| e.id == id)
                .map(|e| e.path.clone())
                .collect();
            assert_eq!(
                scopes,
                vec![vec![("admin-dns-domain".to_string(), 1)]],
                "{id} must render once, on the pending domain's row"
            );
        }
        // The challenge TXT rides the ordinary record component.
        assert!(
            texts(&els, "admin-dns-record-value").contains(&"tokenvalue-deadbeef"),
            "the challenge value must render through admin-dns-record"
        );
        // Only the pending row's issue button is disabled.
        let enabled: Vec<bool> = els
            .iter()
            .filter(|e| e.id == "admin-dns-cert-issue-button")
            .map(|e| e.enabled)
            .collect();
        assert_eq!(enabled, vec![true, false]);
    }

    /// With no held credential there is no controlled zone to re-home the
    /// challenge into, so the delegate button paints **disabled** rather than
    /// absent — an absent affordance reads to the driver as "this app never built
    /// it" — and the reason rides the label (a terminal has no tooltip).
    #[test]
    fn delegate_button_is_disabled_and_says_why_without_a_credential() {
        let app = app_with(
            Some(dns_snapshot(vec![matrix("a.test", Vec::new())])),
            Some(domains_snapshot(
                vec![domain_row("a.test", true)],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        let btn = els
            .iter()
            .find(|e| e.id == "admin-dns-cert-delegate-button")
            .expect("the delegate affordance must exist");
        assert!(!btn.enabled);
        assert_eq!(btn.text, t::cert::DELEGATE_NO_ZONES);
        assert!(texts(&els, "admin-dns-cert-delegate-zone-select").is_empty());
    }

    /// With a credential held, the delegate form is a reveal: the zone picker
    /// offers exactly the covered zones (deduped + sorted) and nothing paints
    /// until the button is clicked.
    #[test]
    fn delegate_form_reveals_with_the_credential_covered_zones() {
        let mut snap = dns_snapshot(vec![matrix("a.test", Vec::new())]);
        snap.credentials = vec![
            CredentialSummary {
                provider_id: "cloudflare".to_string(),
                zones: vec!["z2.test".to_string(), "z1.test".to_string()],
                label: "cloudflare".to_string(),
            },
            CredentialSummary {
                provider_id: "hetzner".to_string(),
                zones: vec!["z1.test".to_string()],
                label: "hetzner".to_string(),
            },
        ];
        let mut app = app_with(
            Some(snap),
            Some(domains_snapshot(
                vec![domain_row("a.test", true)],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        assert!(
            els.iter()
                .any(|e| e.id == "admin-dns-cert-delegate-button" && e.enabled)
        );
        assert!(texts(&els, "admin-dns-cert-delegate-submit-button").is_empty());

        app.admin.dns_delegate_open = Some("a.test".to_string());
        app.admin.dns_delegate_zone = "z1.test".to_string();
        let els = dns_elements(&app.admin);
        assert_eq!(
            options_of(&els, "admin-dns-cert-delegate-zone-select"),
            vec!["z1.test".to_string(), "z2.test".to_string()],
            "sorted + deduped across credentials"
        );
        assert!(
            els.iter()
                .any(|e| e.id == "admin-dns-cert-delegate-submit-button" && e.enabled)
        );
        assert_eq!(
            texts(&els, "admin-dns-cert-delegate-cancel-button").len(),
            1
        );
        // The reveal replaces the button, so there is never both.
        assert!(texts(&els, "admin-dns-cert-delegate-button").is_empty());
    }

    /// An existing delegation replaces the form with the one-time CNAME (rendered
    /// through the shared record component) + a remove affordance.
    #[test]
    fn an_existing_delegation_shows_its_cname_and_a_remove_affordance() {
        let mut snap = dns_snapshot(vec![matrix("a.test", Vec::new())]);
        snap.delegations = vec![DelegationView {
            domain: "a.test".to_string(),
            cname: record(
                "_acme-challenge.a.test",
                "CNAME",
                "_acme-challenge.a.test.zone.test.",
            ),
        }];
        let app = app_with(
            Some(snap),
            Some(domains_snapshot(
                vec![domain_row("a.test", true)],
                Vec::new(),
            )),
        );
        let els = dns_elements(&app.admin);
        assert_eq!(
            texts(&els, "admin-dns-cert-remove-delegation-button").len(),
            1
        );
        assert!(texts(&els, "admin-dns-cert-delegate-button").is_empty());
        assert!(
            texts(&els, "admin-dns-record-value").contains(&"_acme-challenge.a.test.zone.test."),
            "the one-time CNAME must render through admin-dns-record"
        );
    }

    /// A pre-dispatch action failure (resolving the cert-delivery target nest)
    /// still reaches `error-message`, and outranks both snapshots' errors — it is
    /// the most recent thing the admin did.
    #[test]
    fn a_pre_dispatch_action_failure_reaches_error_message_first() {
        let mut dns = dns_snapshot(Vec::new());
        dns.error = Some("list_records failed".to_string());
        let mut app = app_with(Some(dns), Some(domains_snapshot(Vec::new(), Vec::new())));
        app.admin.dns_action_error = Some("resolve target nest for a.test: no pairing".to_string());
        assert_eq!(
            app.error_line_text().as_deref(),
            Some("resolve target nest for a.test: no pairing")
        );
    }
}
