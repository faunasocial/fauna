//! **`NestRetireMachine`** — retiring an app-provisioned nest from the app
//! (`docs/goal/behavior/nest-retirement.md` § Where logic lives).
//!
//! A separate machine type rather than new state on [`OnboardingMachine`]: it
//! needs exactly this crate's dependency set — provider dispatch, the
//! fake-cloud base-URL seam, the UniFFI and `fauna-wasm-onboarding` binding
//! faces (it must load **pre-auth**) — while `OnboardingMachine` is already
//! the largest machine in the tree.
//!
//! Three questions, answered **with no reachable nest**, because the commonest
//! reason to retire a box is that it is already broken or gone: *which servers
//! did Fauna create in my cloud account?*, *delete this one and clean up after
//! it*, *give me my domain's transfer code*. The machine never talks to a
//! nest: everything it knows about local state — candidate domains, the
//! current box's address, the held DNS credential — the app passes **in** at
//! construction ([`RetireInputs`]).
//!
//! **Credential stance** (§ Credential stance): the VPS token is typed in
//! every time and never persisted. It lives in machine memory for the page's
//! lifetime and is dropped on exit ([`NestRetireMachine::cancel`]) — a
//! persisted token would be unreachable exactly when it is needed (the nest
//! holding the account's rows may be the box being destroyed), and a token that can
//! create servers and spend money is the wrong thing to keep for a
//! once-in-a-deployment action.
//!
//! **DNS first, then the server** (§ DNS cleanup — scope and order). Deleting
//! the server first and failing before the DNS step strands `A` records on an
//! address the provider will hand to a stranger — a dangling-DNS takeover —
//! and the box has left the list, so the view can no longer reach the cleanup.
//! DNS-first fails safe: a crash leaves a listed box with some records gone,
//! and re-running converges, because `find_records` + `delete_record` +
//! `delete_server` are all idempotent.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use fauna_core::secret::SecretString;
use fauna_provisioning::dispatch::{
    Credentials, DnsDispatch, VpsDispatch, dns_provider, parse_provider_id, registrar,
    supports_auth_code, vps_provider,
};
use fauna_provisioning::dns::DnsProvider;
use fauna_provisioning::registrar::bundled::AuthCode;
use fauna_provisioning::retire_dns::{
    LeftoverReason, PlannedRemoval, RetireDnsPlan, plan_dns_cleanup, plan_secondary_dns_cleanup,
};
use fauna_provisioning::vps::{ManagedServer, VpsInstance, VpsProvider};
use serde::{Deserialize, Serialize};

use crate::hosted_auth::PendingDeviceAuth;
use crate::state::{FieldMetaPlain, HostedAuthPrompt, HostedAuthState, LocalizedText};

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// Page states, in the order the view walks them (§ Layout & flow).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RetirePhase {
    /// The `vps_config` provider row, credential form and verify button,
    /// reused by ID — no region or server-type controls.
    Credentials,
    /// `verify` + `list_managed_servers` in flight.
    Listing,
    /// One row per listed server.
    List,
    /// Type-the-name confirm.
    Confirm,
    /// The two step rows: DNS, then Server.
    Running,
    /// Outcome, plus any DNS left to remove by hand.
    Done,
}

/// The transfer-authorization-code affordance on a selected row
/// (§ Transfer authorization code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TransferCodeState {
    /// This provider has no registrar adapter with an auth-code call, or the
    /// row has no verified domain. The row carries a one-line note instead of
    /// a button — never a button that would fail.
    Unsupported,
    /// Supported and not yet asked for.
    Idle,
    /// The call is in flight.
    Fetching,
    /// The code, held in memory only and cleared on page exit.
    Code { code: String },
    /// A registry transfer lock still applies; this is when it lifts. Never a
    /// refusal (`bundled-provider-api.md` § Exit guarantee 1).
    AvailableAfter { when: String },
}

/// One listed server, as the view renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ManagedServerRow {
    pub server_id: String,
    pub name: String,
    pub ipv4: Option<String>,
    /// The box's IPv6, when the provider lists one — what scopes the `AAAA`
    /// removals, exactly as `ipv4` scopes the `A`.
    pub ipv6: Option<String>,
    /// The attributed domain — **present only when verified** (§ Box → domain
    /// attribution). Absent means DNS cleanup is skipped for this row and the
    /// transfer-code affordance is gone; the server is still deletable.
    pub domain: Option<String>,
    /// Every **other** domain verified to point at this box — the secondary
    /// local domains the nest served, whose apex `A` the person's zones or
    /// public DNS still resolve to this address (§ DNS cleanup — several
    /// domains). Each is planned, or listed by hand, beside the attributed
    /// domain; never a transfer-code target. Empty when the box has no IPv4.
    pub secondary_domains: Vec<String>,
    /// `false` only where the provider has no label facility (OVH), so the row
    /// could not be proven fauna-provisioned. The view shows an unmarked note.
    pub marked: bool,
    /// This session's own nest — the box the app passed an address for.
    pub current: bool,
    pub transfer_code: TransferCodeState,
}

/// The two steps of a retire run, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RetireStep {
    Dns,
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum StepState {
    Pending,
    Running,
    Done,
    /// The step could not run at all — no verified domain, or no usable DNS
    /// credential. Not a failure: the run continues to the server.
    Skipped {
        why: String,
    },
    Failed {
        cause: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RetireStepRow {
    pub step: RetireStep,
    pub state: StepState,
}

/// A record the run will remove, or has removed — the view's DNS plan line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsPlanLine {
    pub name: String,
    pub record_type: String,
    pub value: String,
    /// `true` once the DNS step has walked this line and the zone holds no
    /// record matching it — removed by this run, or already gone. A DNS step
    /// that fails part way leaves the lines it never reached `false`, which is
    /// what lets *delete the server anyway* name exactly what is left.
    pub removed: bool,
}

/// Why a by-hand record is on the list — what the view keys its text on, so
/// the urgent records read as urgent rather than as four more stale `TXT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum LeftoverKind {
    /// Shares a name with non-Fauna records and carries no pointer to the box:
    /// harmless stale, remove at leisure.
    SharedName,
    /// Still points at the box being destroyed, and no credential could remove
    /// it: remove this one *before* the address is released.
    PointsAtBox,
}

/// A record deliberately left in place, for the person to remove by hand.
///
/// `value` is empty where the value is not Fauna's to reconstruct (the `TXT`
/// rrsets and the `TLSA`), exactly as [`DnsPlanLine`] leaves the `TLSA`'s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LeftoverLine {
    pub name: String,
    pub record_type: String,
    pub value: String,
    pub kind: LeftoverKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RetireSnapshot {
    pub phase: RetirePhase,
    pub servers: Vec<ManagedServerRow>,
    /// The selected row's `server_id`.
    pub selected: Option<String>,
    pub steps: Vec<RetireStepRow>,
    /// What the DNS step will remove — shown in the confirm summary so the
    /// person sees the plan before agreeing to it.
    pub dns_plan: Vec<DnsPlanLine>,
    pub leftover_records: Vec<LeftoverLine>,
    /// What the person has typed into the confirm field.
    pub confirm_name: String,
    /// `true` once `confirm_name` exactly matches the selected server's name.
    pub confirm_enabled: bool,
    /// Whether *delete the server anyway* is offered — a failed DNS step only.
    pub force_server_offered: bool,
    pub error: Option<String>,
    /// The row a finished run deleted — it has left `servers` by then, and
    /// the done state still names it (and says whether it was this session's
    /// own box, which is what sends the app to its launch flow).
    pub retired: Option<ManagedServerRow>,
}

impl RetireSnapshot {
    fn initial() -> Self {
        Self {
            phase: RetirePhase::Credentials,
            servers: Vec::new(),
            selected: None,
            steps: vec![
                RetireStepRow {
                    step: RetireStep::Dns,
                    state: StepState::Pending,
                },
                RetireStepRow {
                    step: RetireStep::Server,
                    state: StepState::Pending,
                },
            ],
            dns_plan: Vec::new(),
            leftover_records: Vec::new(),
            confirm_name: String::new(),
            confirm_enabled: false,
            force_server_offered: false,
            error: None,
            retired: None,
        }
    }

    fn step_mut(&mut self, step: RetireStep) -> &mut RetireStepRow {
        self.steps
            .iter_mut()
            .find(|r| r.step == step)
            .expect("both steps are seeded in `initial`")
    }

    fn selected_row(&self) -> Option<&ManagedServerRow> {
        let id = self.selected.as_deref()?;
        self.servers.iter().find(|s| s.server_id == id)
    }
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// A DNS credential the app already holds (`fauna.state.dns`), passed **in**
/// because the machine never reads config and never talks to a nest
/// (§ Credential stance, preference (b)).
#[derive(Debug, Clone, Default)]
pub struct HeldDnsCredential {
    pub provider_id: String,
    pub fields: BTreeMap<String, SecretString>,
}

impl HeldDnsCredential {
    /// Every credential `fauna.state.dns` holds, in its own order — a
    /// deployment may hold one per (provider, set of zones), and a domain is
    /// cleaned through whichever one's zone covers it (§ Credential stance:
    /// resolved per domain, on *covering the domain*). The one projection the
    /// apps share, so none re-derives the field bag.
    pub fn all_from(dns: &fauna_core::data::DnsConfig) -> Vec<Self> {
        dns.credentials
            .iter()
            .map(|c| Self {
                provider_id: c.provider_id.clone(),
                fields: c.fields.iter().cloned().collect(),
            })
            .collect()
    }
}

/// Everything the app knows that the machine cannot find out for itself. All
/// optional — the view works from a cold launch with none of it.
#[derive(Debug, Clone, Default)]
pub struct RetireInputs {
    /// Domains from local state (the signed-in account's nest domain, the
    /// pending-provision slot; from `admin-nest`, the nest's active local
    /// domains) — attribution candidate (2), and the other-verified-domain
    /// scan's app-passed half (§ DNS cleanup → *Several domains* (1)).
    /// Settable afterwards too ([`NestRetireMachine::add_candidate_domains`]).
    pub candidate_domains: Vec<String>,
    /// The current nest's IPv4, for the *current* badge.
    pub current_ipv4: Option<String>,
    /// Every held `fauna.state.dns` credential ([`HeldDnsCredential::all_from`]).
    /// Settable afterwards too ([`NestRetireMachine::set_held_dns`]).
    pub held_dns: Vec<HeldDnsCredential>,
    /// The e2e fake-cloud seam: every provider adapter is pointed here when
    /// set, exactly as the wizard's `set_provider_base_urls` does.
    pub provider_base_url: Option<String>,
    /// The unit-test seam for the public-DNS attribution read: the DoH
    /// endpoint is pointed here when set, else Cloudflare's. Deliberately
    /// **not** on the binding-face constructor — the resolver is a Rust
    /// constant, not something an app chooses.
    pub doh_base_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Machine
// ---------------------------------------------------------------------------

struct Inner {
    provider_id: Option<String>,
    /// The VPS token — memory only, for the page's lifetime, dropped on exit.
    creds: BTreeMap<String, SecretString>,
    /// Raw listing rows, kept beside the rendered ones so the run has the
    /// server's real addresses without trusting what the view echoed back.
    raw: Vec<ManagedServer>,
    /// Every usable DNS credential with the zones it reported at verify time,
    /// in preference order (§ Credential stance) — how a domain finds the
    /// credential whose zone covers it, and attribution candidate (3)'s source.
    /// Empty when no credential is usable: DNS cleanup is not attempted and
    /// the page lists what to remove by hand.
    credentials: Vec<UsableCredential>,
    /// The server the typed-name gate was passed for, set by `confirm` — the
    /// only id `retry` and `force_server` ever act on (§ Confirm shape: the
    /// gate binds the run to the confirmed server). `None` until a confirm
    /// passes; cleared by `back`, `cancel` and `select_provider`.
    armed: Option<String>,
    /// Each `hosted-auth` credential field's sign-in state (the bundled
    /// provider's device flow — the same controls `vps_config` renders).
    hosted_auth: BTreeMap<String, HostedAuthState>,
    /// A device-authorization attempt between `hosted_auth_begin` and
    /// `hosted_auth_wait`, per field.
    hosted_pending: BTreeMap<String, PendingDeviceAuth>,
    /// This session's nest's IPv4, for the *current* badge — seeded from
    /// [`RetireInputs::current_ipv4`] and settable afterwards, because an app
    /// learns it by resolving its nest's host, which may finish after the
    /// page opened.
    current_ipv4: Option<String>,
    /// Attribution candidate (2), and the other-verified-domain scan's
    /// app-passed half — seeded from
    /// [`RetireInputs::candidate_domains`] and extendable afterwards, because
    /// the admin entry reads the nest's local-domain list after the page opens.
    candidate_domains: Vec<String>,
    /// Preference (b)'s credentials — seeded from [`RetireInputs::held_dns`]
    /// and replaceable afterwards (the admin entry's live custody read).
    /// Read at `verify`; dropped on exit with the token.
    held_dns: Vec<HeldDnsCredential>,
    /// The app has said more inputs are on their way
    /// ([`NestRetireMachine::expect_app_inputs`]); verify waits for them.
    inputs_pending: bool,
    snap: RetireSnapshot,
}

/// One DNS credential the run may use, with the zones its `verify()` reported.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UsableCredential {
    choice: DnsChoice,
    /// `(zone name, zone id)`.
    zones: Vec<(String, String)>,
}

/// The credential that can touch `domain`'s zone, resolved per domain — the
/// spec's own words are *a zone covering the domain*, so a token holding some
/// unrelated zone never shadows a held credential holding the right one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ZoneCover {
    choice: DnsChoice,
    zone_id: String,
    /// Every zone of that credential — what turns an absolute name relative.
    zones: Vec<(String, String)>,
}

/// Which DNS credential a zone is reached through (§ Credential stance, in
/// preference order).
#[derive(Debug, Clone, PartialEq, Eq)]
enum DnsChoice {
    /// (a) the token just entered — its provider also has DNS capability and
    /// reported a zone covering the domain.
    EnteredToken,
    /// (b) a held `fauna.state.dns` credential the app passed in, by its index
    /// in the held list.
    Held(usize),
}

/// One instance per `nest_retire` page visit.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct NestRetireMachine {
    inputs: RetireInputs,
    inner: Mutex<Inner>,
    http: reqwest::Client,
}

impl NestRetireMachine {
    /// The Rust-native constructor: everything the app knows, typed
    /// ([`RetireInputs`]). The UniFFI/wasm faces below funnel into this.
    pub fn new_with_inputs(inputs: RetireInputs) -> Arc<Self> {
        let current_ipv4 = inputs.current_ipv4.clone();
        let candidate_domains = inputs.candidate_domains.clone();
        let held_dns = inputs.held_dns.clone();
        Arc::new(Self {
            inputs,
            inner: Mutex::new(Inner {
                current_ipv4,
                candidate_domains,
                held_dns,
                inputs_pending: false,
                provider_id: None,
                creds: BTreeMap::new(),
                raw: Vec::new(),
                credentials: Vec::new(),
                armed: None,
                hosted_auth: BTreeMap::new(),
                hosted_pending: BTreeMap::new(),
                snap: RetireSnapshot::initial(),
            }),
            http: reqwest::Client::new(),
        })
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl NestRetireMachine {
    /// The binding-face constructor. Every input is optional — the page opens
    /// cold from `launch_retry` with none of it — and each held DNS credential
    /// arrives as the same `providers.yaml`-keyed JSON bag the wizard's
    /// dispatch already takes, so no new credential wire shape appears:
    /// `held_dns_provider_ids[i]` pairs with `held_dns_creds_json[i]`, one
    /// entry per `fauna.state.dns` credential (a pair missing its other half
    /// is dropped).
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new(
        candidate_domains: Vec<String>,
        current_ipv4: Option<String>,
        held_dns_provider_ids: Vec<String>,
        held_dns_creds_json: Vec<String>,
        provider_base_url: Option<String>,
    ) -> Arc<Self> {
        let held_dns = held_dns_from_json(held_dns_provider_ids, held_dns_creds_json);
        Self::new_with_inputs(RetireInputs {
            candidate_domains,
            current_ipv4,
            held_dns,
            provider_base_url,
            doh_base_url: None,
        })
    }

    pub fn snapshot(&self) -> RetireSnapshot {
        self.inner.lock().expect("retire mutex").snap.clone()
    }

    // -- provider / credential entry -------------------------------------

    pub fn select_provider(&self, provider_id: String) {
        let mut inner = self.inner.lock().expect("retire mutex");
        if inner.provider_id.as_deref() != Some(provider_id.as_str()) {
            // A different provider means a different account: nothing listed
            // under the old one may survive the switch.
            inner.creds.clear();
            inner.raw.clear();
            inner.hosted_auth.clear();
            inner.hosted_pending.clear();
            inner.snap = RetireSnapshot::initial();
        }
        // Disarmed whether or not the provider changed: re-entering the
        // credential form is never a way back into a confirmed run.
        inner.armed = None;
        inner.provider_id = Some(provider_id);
    }

    pub fn set_credential_field(&self, field_id: String, value: String) {
        let mut inner = self.inner.lock().expect("retire mutex");
        inner.creds.insert(field_id, SecretString::from(value));
        inner.snap.error = None;
    }

    /// Back out of the page. **Drops the token** and every fetched code — the
    /// credential stance is that neither outlives the visit.
    pub fn cancel(&self) {
        let mut inner = self.inner.lock().expect("retire mutex");
        inner.creds.clear();
        inner.raw.clear();
        inner.credentials.clear();
        inner.provider_id = None;
        inner.armed = None;
        inner.hosted_auth.clear();
        inner.hosted_pending.clear();
        inner.held_dns.clear();
        inner.snap = RetireSnapshot::initial();
    }

    /// Where a `hosted-auth` credential field's sign-in stands — `Idle` for a
    /// field never started. The retire view reuses `vps_config`'s credential
    /// controls, so a bundled provider signs in here exactly as it does there.
    pub fn hosted_auth_state(&self, field_id: String) -> HostedAuthState {
        self.inner
            .lock()
            .expect("retire mutex")
            .hosted_auth
            .get(&field_id)
            .cloned()
            .unwrap_or(HostedAuthState::Idle)
    }

    /// Whether the sign-in button is pressable: the form's `base-url` is
    /// filled in and no attempt on this field is mid-flight.
    pub fn hosted_auth_can_begin(&self, field_id: String) -> bool {
        self.hosted_auth_base_url().is_some()
            && !matches!(
                self.hosted_auth_state(field_id),
                HostedAuthState::Pending { .. }
            )
    }

    /// The `hosted-auth` button's label — the wizard's one mapping.
    pub fn hosted_auth_button_text(&self, field_id: String) -> String {
        crate::hosted_auth::button_text(&self.hosted_auth_state(field_id))
    }

    /// Back from a later phase to the one before it. From `List` this returns
    /// to the credential form; from `Confirm`, to the list. Either move
    /// disarms; a run in flight or failed is left by `cancel`, not by `back`.
    pub fn back(&self) {
        let mut inner = self.inner.lock().expect("retire mutex");
        match inner.snap.phase {
            RetirePhase::Confirm => {
                inner.armed = None;
                let s = &mut inner.snap;
                s.phase = RetirePhase::List;
                s.confirm_name.clear();
                s.confirm_enabled = false;
                s.error = None;
            }
            RetirePhase::List => {
                inner.armed = None;
                let s = &mut inner.snap;
                s.phase = RetirePhase::Credentials;
                s.selected = None;
                s.error = None;
            }
            _ => {}
        }
    }

    /// Select a row — in the list state only. Clears any code fetched for a
    /// previously selected row — a code belongs to the domain it was fetched
    /// for. Refused in every other phase, so nothing re-targets a confirm
    /// summary or a run once it names a server.
    pub fn select(&self, server_id: String) {
        self.with_snap(|s| {
            if s.phase != RetirePhase::List {
                return;
            }
            s.selected = Some(server_id);
            s.confirm_name.clear();
            s.confirm_enabled = false;
            s.error = None;
        });
    }

    /// The typed-name gate (§ Confirm shape): the confirm button enables on an
    /// **exact** match with the selected server's provider-side name, on every
    /// provider — one uniform shape, and it is what discharges the mandatory
    /// per-box confirm for unmarked (OVH) rows.
    pub fn set_confirm_name(&self, typed: String) {
        self.with_snap(|s| {
            let expected = s.selected_row().map(|r| r.name.clone());
            s.confirm_enabled = expected.is_some_and(|name| !name.is_empty() && name == typed);
            s.confirm_name = typed;
        });
    }

    /// Move to the confirm state, computing the DNS plan the summary shows.
    pub fn begin_confirm(&self) {
        let mut inner = self.inner.lock().expect("retire mutex");
        let Some(row) = inner.snap.selected_row().cloned() else {
            inner.snap.error = Some("no server selected".into());
            return;
        };
        let plan = self.plan_for(&row, &inner.credentials);
        inner.snap.dns_plan = plan
            .removals
            .iter()
            .map(|r| DnsPlanLine {
                name: r.name.clone(),
                record_type: r.record_type.clone(),
                value: r.value.clone(),
                removed: false,
            })
            .collect();
        inner.snap.leftover_records = plan
            .leftovers
            .iter()
            .map(|l| LeftoverLine {
                name: l.name.clone(),
                record_type: l.record_type.clone(),
                value: l.value.clone(),
                kind: match l.reason {
                    LeftoverReason::SharedName => LeftoverKind::SharedName,
                    LeftoverReason::PointsAtBox => LeftoverKind::PointsAtBox,
                },
            })
            .collect();
        inner.snap.confirm_name.clear();
        inner.snap.confirm_enabled = false;
        inner.snap.phase = RetirePhase::Confirm;
        inner.snap.error = None;
    }

    // -- dispatch helpers -------------------------------------------------
}

// ---------------------------------------------------------------------------
// The current box, and the view's sentences
//
// Every sentence that says what dies, what the DNS plan is, or what is left
// to remove by hand is composed here, once, as `LocalizedText` the apps
// resolve through their own i18n pipelines — so the seven apps cannot say
// different things about an irreversible act (priority #2).
// ---------------------------------------------------------------------------

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl NestRetireMachine {
    /// The provider the credential form is for, once one is picked.
    pub fn selected_provider(&self) -> Option<String> {
        self.inner.lock().expect("retire mutex").provider_id.clone()
    }

    /// The selected provider's VPS credential fields — the same filter
    /// `vps_config` renders (`OnboardingMachine::visible_vps_fields`), since
    /// the retire view reuses those controls by id.
    pub fn visible_fields(&self) -> Vec<FieldMetaPlain> {
        let Some(pid) = self.selected_provider() else {
            return Vec::new();
        };
        fauna_provisioning::providers_generated::PROVIDERS
            .iter()
            .find(|p| p.id.as_str() == pid)
            .map(|p| {
                p.fields
                    .iter()
                    .filter(|f| {
                        f.kinds
                            .contains(&fauna_provisioning::providers_generated::Capability::Vps)
                    })
                    .map(FieldMetaPlain::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the verify button is pressable: a provider is picked, every
    /// required field is filled in, no listing is in flight, and no input the
    /// app announced is still on its way.
    pub fn can_verify(&self) -> bool {
        let fields = self.visible_fields();
        let inner = self.inner.lock().expect("retire mutex");
        !inner.inputs_pending
            && inner.provider_id.is_some()
            && inner.snap.phase == RetirePhase::Credentials
            && fields.iter().all(|f| {
                !f.required
                    || inner
                        .creds
                        .get(&f.id)
                        .is_some_and(|v| !v.as_str().is_empty())
            })
    }

    /// The app is reading more inputs after the page opened (the admin
    /// entry's live custody read and local-domain list): hold verify until
    /// [`Self::app_inputs_landed`], because `verify` is where the credentials
    /// and candidates are read, and a verify ahead of them would clean less
    /// than the person's own config allows.
    pub fn expect_app_inputs(&self) {
        self.inner.lock().expect("retire mutex").inputs_pending = true;
    }

    /// The announced inputs have landed — or their read failed, which keeps
    /// what the page opened with. Verify is pressable again.
    pub fn app_inputs_landed(&self) {
        self.inner.lock().expect("retire mutex").inputs_pending = false;
    }

    /// Add attribution candidates the app learned after the page opened — the
    /// admin entry's active local-domain list (§ DNS cleanup → *Several
    /// domains* (1)). Read at `verify`; a domain already held is not repeated.
    pub fn add_candidate_domains(&self, domains: Vec<String>) {
        let mut inner = self.inner.lock().expect("retire mutex");
        for domain in domains {
            let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
            if !domain.is_empty() && !inner.candidate_domains.contains(&domain) {
                inner.candidate_domains.push(domain);
            }
        }
    }

    /// Replace the held `fauna.state.dns` credentials (§ Credential stance,
    /// preference (b)) with ones the app read after the page opened — the
    /// same index-paired JSON bags as the constructor. Read at `verify`.
    pub fn set_held_dns_json(
        &self,
        held_dns_provider_ids: Vec<String>,
        held_dns_creds_json: Vec<String>,
    ) {
        self.set_held_dns(held_dns_from_json(
            held_dns_provider_ids,
            held_dns_creds_json,
        ));
    }

    /// Record this session's nest's IPv4 once the app has resolved it, and
    /// re-mark any rows already listed. `None` clears the badge.
    pub fn set_current_ipv4(&self, ipv4: Option<String>) {
        let mut inner = self.inner.lock().expect("retire mutex");
        inner.current_ipv4 = ipv4;
        let current = inner.current_ipv4.clone();
        for row in &mut inner.snap.servers {
            row.current = matches!((&row.ipv4, &current), (Some(a), Some(b)) if a == b);
        }
    }

    /// The confirm summary (§ Confirm shape), one sentence per line, in the
    /// order the view shows them: what is deleted and where; the attributed
    /// domain and every other verified domain; that the data cannot be
    /// recovered except from a backup; on the current box, that this session's
    /// nest stops existing; and the DNS plan — the records removed first, or
    /// that none will be — followed by what is left to remove by hand.
    ///
    /// The `provider` argument of the first line is the provider's display
    /// **key**, so render it with nested resolution.
    pub fn confirm_summary(&self) -> Vec<LocalizedText> {
        let inner = self.inner.lock().expect("retire mutex");
        let s = &inner.snap;
        let Some(row) = s.selected_row() else {
            return Vec::new();
        };
        let provider = inner
            .provider_id
            .as_deref()
            .and_then(|id| {
                fauna_provisioning::providers_generated::PROVIDERS
                    .iter()
                    .find(|p| p.id.as_str() == id)
            })
            .map(|p| p.display_name_key.to_string())
            .unwrap_or_default();
        let mut out = vec![LocalizedText::key_args(
            "onboarding.retire.confirm_what",
            [
                ("name", row.name.clone()),
                ("provider", provider),
                ("address", row.ipv4.clone().unwrap_or_default()),
            ],
        )];
        if let Some(domain) = &row.domain {
            out.push(LocalizedText::key_arg(
                "onboarding.retire.confirm_domain",
                "domain",
                domain.clone(),
            ));
        }
        if !row.secondary_domains.is_empty() {
            out.push(LocalizedText::key_arg(
                "onboarding.retire.confirm_secondary_domains",
                "domains",
                row.secondary_domains.join(", "),
            ));
        }
        out.push(LocalizedText::key("onboarding.retire.confirm_destroyed"));
        if row.current {
            out.push(LocalizedText::key("onboarding.retire.confirm_current"));
        }
        if s.dns_plan.is_empty() {
            out.push(LocalizedText::key("onboarding.retire.confirm_dns_none"));
        } else {
            let records: Vec<String> = s
                .dns_plan
                .iter()
                .map(|l| record_text(&l.name, &l.record_type, &l.value))
                .collect();
            out.push(LocalizedText::key_arg(
                "onboarding.retire.confirm_dns_removals",
                "records",
                records.join("; "),
            ));
        }
        if !s.leftover_records.is_empty() {
            let records: Vec<String> = s
                .leftover_records
                .iter()
                .map(|l| record_text(&l.name, &l.record_type, &l.value))
                .collect();
            out.push(LocalizedText::key_arg(
                "onboarding.retire.confirm_by_hand",
                "records",
                records.join("; "),
            ));
        }
        out
    }

    /// The done state's by-hand list, one line per record: the ones still
    /// pointing at the deleted box first, worded as urgent, then the stale
    /// shared-name `TXT` (§ DNS cleanup). A single "nothing left" line when
    /// the list is empty.
    pub fn leftover_lines(&self) -> Vec<LocalizedText> {
        let inner = self.inner.lock().expect("retire mutex");
        let lines = &inner.snap.leftover_records;
        if lines.is_empty() {
            return vec![LocalizedText::key("onboarding.retire.leftover_none")];
        }
        lines
            .iter()
            .map(|l| {
                let key = match l.kind {
                    LeftoverKind::PointsAtBox => "onboarding.retire.leftover_points_at_box",
                    LeftoverKind::SharedName => "onboarding.retire.leftover_shared_name",
                };
                LocalizedText::key_arg(
                    key,
                    "record",
                    record_text(&l.name, &l.record_type, &l.value),
                )
            })
            .collect()
    }

    /// The transfer-code line a row carries in place of (or beside) its
    /// button: the lock-lifts date for `AvailableAfter`, the go-to-your-
    /// registrar note for `Unsupported` on an attributed row, "asking" while
    /// fetching. `None` when the button alone says it (`Idle`) or the code
    /// itself is shown, and for an unattributed row (nothing to transfer).
    pub fn transfer_code_note(&self, server_id: String) -> Option<LocalizedText> {
        let inner = self.inner.lock().expect("retire mutex");
        let row = inner
            .snap
            .servers
            .iter()
            .find(|r| r.server_id == server_id)?;
        match &row.transfer_code {
            TransferCodeState::AvailableAfter { when } => Some(LocalizedText::key_arg(
                "onboarding.retire.transfer_code_available_after",
                "when",
                when.clone(),
            )),
            TransferCodeState::Fetching => Some(LocalizedText::key(
                "onboarding.retire.transfer_code_fetching",
            )),
            TransferCodeState::Unsupported if row.domain.is_some() => Some(LocalizedText::key(
                "onboarding.retire.transfer_code_from_registrar",
            )),
            _ => None,
        }
    }

    /// A step row's name.
    pub fn step_label(&self, step: RetireStep) -> LocalizedText {
        LocalizedText::key(match step {
            RetireStep::Dns => "onboarding.retire.step_dns",
            RetireStep::Server => "onboarding.retire.step_server",
        })
    }

    /// Why a skipped step was skipped — `None` for any other state.
    pub fn step_note(&self, state: StepState) -> Option<LocalizedText> {
        match state {
            StepState::Skipped { why } => Some(LocalizedText::key(format!(
                "onboarding.retire.step_skipped_{why}"
            ))),
            _ => None,
        }
    }
}

/// One DNS record as the view names it: `name TYPE value`, the value left off
/// where Fauna cannot reconstruct it (the `TXT` rrsets, the floor `TLSA`).
fn record_text(name: &str, record_type: &str, value: &str) -> String {
    if value.is_empty() {
        format!("{name} {record_type}")
    } else {
        format!("{name} {record_type} {value}")
    }
}

impl StepState {
    /// The shared provisioning-progress status this step state renders as —
    /// the retire view reuses the `provisioning-step-row` component, so it
    /// reuses its glyph and status vocabulary too.
    pub fn progress_status(&self) -> fauna_provisioning::progress::StepStatus {
        use fauna_provisioning::progress::StepStatus;
        match self {
            StepState::Pending => StepStatus::Pending,
            StepState::Running => StepStatus::Running,
            StepState::Done => StepStatus::Succeeded,
            StepState::Skipped { .. } => StepStatus::Skipped,
            StepState::Failed { .. } => StepStatus::Failed,
        }
    }
}

// ---------------------------------------------------------------------------
// Async actions
// ---------------------------------------------------------------------------

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl NestRetireMachine {
    /// Step 1 of a `hosted-auth` field's sign-in: POST the device-authorization
    /// request to the form's `base-url`. `Some` carries what the app opens and
    /// shows; `None` means the attempt failed and the field's state says why.
    /// Follow with [`Self::hosted_auth_wait`].
    pub async fn hosted_auth_begin(&self, field_id: String) -> Option<HostedAuthPrompt> {
        match crate::hosted_auth::begin(&self.http, self.hosted_auth_base_url()).await {
            Ok((prompt, pending)) => {
                let mut inner = self.inner.lock().expect("retire mutex");
                inner.hosted_pending.insert(field_id.clone(), pending);
                inner.hosted_auth.insert(
                    field_id,
                    HostedAuthState::Pending {
                        user_code: prompt.user_code.clone(),
                        verification_url: prompt.verification_url.clone(),
                    },
                );
                Some(prompt)
            }
            Err(message) => {
                self.inner
                    .lock()
                    .expect("retire mutex")
                    .hosted_auth
                    .insert(field_id, HostedAuthState::Failed { message });
                None
            }
        }
    }

    /// Step 2: poll until the user approves — the token lands in the credential
    /// bag under `field_id` (memory only, like every retire credential) and the
    /// field flips to `Connected` — or the attempt ends (`Failed`).
    pub async fn hosted_auth_wait(&self, field_id: String) {
        let pending = self
            .inner
            .lock()
            .expect("retire mutex")
            .hosted_pending
            .remove(&field_id);
        let Some(pending) = pending else {
            return;
        };
        let label = format!("retire/{field_id}");
        let outcome = crate::hosted_auth::wait(&self.http, pending, &label).await;
        let mut inner = self.inner.lock().expect("retire mutex");
        match outcome {
            Ok(token) => {
                inner
                    .creds
                    .insert(field_id.clone(), SecretString::from(token));
                inner
                    .hosted_auth
                    .insert(field_id, HostedAuthState::Connected);
                inner.snap.error = None;
            }
            Err(message) => {
                inner
                    .hosted_auth
                    .insert(field_id, HostedAuthState::Failed { message });
            }
        }
    }

    /// Verify the entered credential and list the fauna boxes in the account.
    pub async fn verify(&self) {
        self.with_snap(|s| {
            s.phase = RetirePhase::Listing;
            s.error = None;
        });

        let vps = match self.vps() {
            Ok(v) => v,
            Err(e) => return self.fail_to_credentials(e),
        };
        if let Err(e) = vps.verify(&self.http).await {
            return self.fail_to_credentials(format!("{e}"));
        }
        let servers = match vps.list_managed_servers(&self.http).await {
            Ok(s) => s,
            Err(e) => return self.fail_to_credentials(format!("{e}")),
        };

        // Resolve every usable DNS credential once, in the documented
        // preference order, and remember the zones each reports — attribution
        // candidate (3)'s source and the zone lookup for every later
        // find/delete, per domain.
        let credentials = self.resolve_dns_credentials().await;

        // A zone's apex does not depend on which box is asking, so one read
        // per candidate serves every listed server.
        let mut apex_cache: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut rows = Vec::with_capacity(servers.len());
        for s in &servers {
            let (domain, secondary_domains) = self
                .verified_domains(&vps, s, &credentials, &mut apex_cache)
                .await;
            let supports_code = self
                .inner
                .lock()
                .expect("retire mutex")
                .provider_id
                .as_deref()
                .and_then(|id| parse_provider_id(id).ok())
                .is_some_and(supports_auth_code);
            rows.push(ManagedServerRow {
                server_id: s.server_id.clone(),
                name: s.name.clone(),
                ipv4: s.ipv4.clone(),
                ipv6: s.ipv6.clone(),
                // The affordance requires a verified attribution: without a
                // domain there is nothing to ask a registrar about.
                transfer_code: if supports_code && domain.is_some() {
                    TransferCodeState::Idle
                } else {
                    TransferCodeState::Unsupported
                },
                current: self.is_current(s.ipv4.as_deref()),
                marked: s.marked,
                domain,
                secondary_domains,
            });
        }

        let mut inner = self.inner.lock().expect("retire mutex");
        inner.raw = servers;
        inner.credentials = credentials;
        inner.snap.servers = rows;
        inner.snap.phase = RetirePhase::List;
    }

    /// Fetch the selected row's transfer authorization code. Both arms are
    /// rendered: the code, or the instant a registry lock lifts.
    pub async fn fetch_transfer_code(&self) {
        let Some(row) = self.with_snap(|s| s.selected_row().cloned()) else {
            return;
        };
        let Some(domain) = row.domain.clone() else {
            return;
        };
        self.set_code_state(&row.server_id, TransferCodeState::Fetching);

        let id = {
            let inner = self.inner.lock().expect("retire mutex");
            inner.provider_id.clone()
        };
        let Some(id) = id.and_then(|i| parse_provider_id(&i).ok()) else {
            return self.set_code_state(&row.server_id, TransferCodeState::Unsupported);
        };
        let Some(reg) = registrar(id, self.creds_bag()) else {
            return self.set_code_state(&row.server_id, TransferCodeState::Unsupported);
        };

        let state = match reg.auth_code(&self.http, &domain).await {
            Ok(Some(AuthCode::Ready(code))) => TransferCodeState::Code { code },
            Ok(Some(AuthCode::AvailableAfter(when))) => TransferCodeState::AvailableAfter { when },
            Ok(None) => TransferCodeState::Unsupported,
            Err(e) => {
                self.with_snap(|s| s.error = Some(format!("{e}")));
                TransferCodeState::Idle
            }
        };
        self.set_code_state(&row.server_id, state);
    }

    /// Run the retire: **DNS first, then the server.**
    ///
    /// Refuses unless the typed-name gate is satisfied. A DNS-step failure
    /// stops the run *before the server is touched* and offers retry or
    /// *delete the server anyway* — never a silent continue, because
    /// continuing is what strands `A` records on an address about to be
    /// reassigned.
    ///
    /// Passing the gate **arms** the run to the selected server: `retry` and
    /// `force_server` act on that id and no other, so the typed name guards
    /// every door into `delete_server`, not only this one.
    pub async fn confirm(&self) {
        let armed = {
            let mut inner = self.inner.lock().expect("retire mutex");
            if inner.snap.phase != RetirePhase::Confirm || !inner.snap.confirm_enabled {
                return;
            }
            let Some(id) = inner.snap.selected.clone() else {
                return;
            };
            inner.armed = Some(id.clone());
            id
        };
        self.run(&armed, true).await;
    }

    /// Re-run after a failed step. Idempotent by construction: `find_records`
    /// + `delete_record` + `delete_server` all converge, so a run that died
    ///   between the steps simply finishes. Acts only on the armed server,
    ///   and only while its run is on screen.
    pub async fn retry(&self) {
        let Some(armed) = self.armed_run(false) else {
            return;
        };
        self.run(&armed, true).await;
    }

    /// Delete the server despite a failed DNS step (§ DNS cleanup: the failure
    /// branch offers *retry* or *delete the server anyway*). Acts only on the
    /// armed server, and only while the offer stands. The not-removed plan
    /// lines join the by-hand list; the dangling-record warning is the view's
    /// to repeat.
    pub async fn force_server(&self) {
        let Some(armed) = self.armed_run(true) else {
            return;
        };
        self.run(&armed, false).await;
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// The zone whose name is `domain` or a parent of it, longest match first — a
/// person may hold both `example.test` and `sub.example.test` as zones.
fn zone_id_for(zones: &[(String, String)], domain: &str) -> Option<String> {
    zones
        .iter()
        .filter(|(name, _)| domain == name || domain.ends_with(&format!(".{name}")))
        .max_by_key(|(name, _)| name.len())
        .map(|(_, id)| id.clone())
}

/// The first usable credential, in preference order, holding a zone that
/// covers `domain` — § Credential stance's *a zone covering the domain*,
/// resolved per domain. `None` when no credential can touch that zone.
fn zone_cover(credentials: &[UsableCredential], domain: &str) -> Option<ZoneCover> {
    credentials.iter().find_map(|c| {
        zone_id_for(&c.zones, domain).map(|zone_id| ZoneCover {
            choice: c.choice.clone(),
            zone_id,
            zones: c.zones.clone(),
        })
    })
}

/// One domain's share of a row's plan: what the run removes through `cover`,
/// or — with no cover — the by-hand form of the same plan.
struct DomainPlan {
    domain: String,
    cover: Option<ZoneCover>,
    plan: RetireDnsPlan,
}

/// Drop repeats while keeping first-seen order — candidates come from three
/// sources that may name the same domain, and the order is the preference.
fn dedup_in_order(items: Vec<String>) -> Vec<String> {
    let mut seen = Vec::with_capacity(items.len());
    for item in items {
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen
}

/// The zone this record name sits in, for turning an absolute name relative.
fn zone_name_for<'a>(zones: &'a [(String, String)], domain: &str) -> Option<&'a str> {
    zones
        .iter()
        .filter(|(name, _)| domain == name || domain.ends_with(&format!(".{name}")))
        .max_by_key(|(name, _)| name.len())
        .map(|(name, _)| name.as_str())
}

/// The apex name as this provider spells it — `@` for a zone-relative
/// provider, the absolute domain otherwise (`registry.md` § The owner-name
/// contract).
fn apex_name(dns: &DnsDispatch, domain: &str, zones: &[(String, String)]) -> String {
    relative_name(dns, domain, domain, zones)
}

/// Turn an absolute record name into what this provider expects: `@` at the
/// apex and a zone-relative label below it for providers that want relative
/// names, the absolute name otherwise.
fn relative_name(
    dns: &DnsDispatch,
    absolute: &str,
    domain: &str,
    zones: &[(String, String)],
) -> String {
    if !dns.record_names_relative_to_zone() {
        return absolute.to_string();
    }
    let zone = zone_name_for(zones, domain).unwrap_or(domain);
    if absolute == zone {
        return "@".to_string();
    }
    absolute
        .strip_suffix(&format!(".{zone}"))
        .unwrap_or(absolute)
        .to_string()
}

/// The binding faces' held-credential form: `provider_ids[i]` pairs with
/// `creds_json[i]`, each a `providers.yaml`-keyed JSON bag. A bag that does not
/// parse keeps its provider with no fields (its `verify()` then fails and it is
/// simply not usable); a pair missing its other half is dropped.
fn held_dns_from_json(
    provider_ids: Vec<String>,
    creds_json: Vec<String>,
) -> Vec<HeldDnsCredential> {
    provider_ids
        .into_iter()
        .zip(creds_json)
        .map(|(provider_id, json)| HeldDnsCredential {
            provider_id,
            fields: serde_json::from_str(&json).unwrap_or_default(),
        })
        .collect()
}

/// Compare a record's value with the planned one, tolerating the trailing dot
/// and the `priority weight port target` prefix providers put on MX/SRV
/// values: the planned value is the *host* those records must point at.
fn value_matches(actual: &str, planned: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('.').to_ascii_lowercase();
    let actual_n = norm(actual);
    let planned_n = norm(planned);
    actual_n == planned_n
        || actual_n
            .split_whitespace()
            .any(|tok| norm(tok) == planned_n)
}

// ---------------------------------------------------------------------------
// Private helpers
//
// Deliberately a plain `impl` block: `uniffi::export` exports every method
// in an annotated block regardless of visibility, so anything taking or
// returning a non-FFI type (a dispatcher, a credential bag, a tuple) has to
// live outside one.
// ---------------------------------------------------------------------------

impl NestRetireMachine {
    /// Replace the held `fauna.state.dns` credentials, typed — the Rust apps'
    /// form of [`Self::set_held_dns_json`] (outside the exported block: the
    /// field bag is not an FFI type).
    pub fn set_held_dns(&self, held: Vec<HeldDnsCredential>) {
        self.inner.lock().expect("retire mutex").held_dns = held;
    }

    fn with_snap<T>(&self, f: impl FnOnce(&mut RetireSnapshot) -> T) -> T {
        f(&mut self.inner.lock().expect("retire mutex").snap)
    }

    /// Is `ipv4` this session's own nest's address?
    fn is_current(&self, ipv4: Option<&str>) -> bool {
        let inner = self.inner.lock().expect("retire mutex");
        matches!((ipv4, inner.current_ipv4.as_deref()), (Some(a), Some(b)) if a == b)
    }

    /// The typed `base-url` a hosted sign-in reads its authorization server
    /// from, when one is filled in.
    fn hosted_auth_base_url(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("retire mutex")
            .creds
            .get(crate::hosted_auth::BASE_URL_FIELD)
            .map(|v| v.as_str().trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// The armed server id, when a follow-up to a confirmed run may act on it:
    /// the run is on screen (`Running`) and — for *delete anyway* — the
    /// failed DNS step offered it. `None` refuses the action.
    fn armed_run(&self, needs_force_offer: bool) -> Option<String> {
        let inner = self.inner.lock().expect("retire mutex");
        let s = &inner.snap;
        if s.phase != RetirePhase::Running || (needs_force_offer && !s.force_server_offered) {
            return None;
        }
        inner.armed.clone()
    }

    /// A row's plan, one domain at a time (§ DNS cleanup — several domains):
    /// the attributed domain's full plan, then one secondary plan per other
    /// verified domain, each the full value-scoped cleanup when some usable
    /// credential holds a zone covering **that** domain, else the by-hand form
    /// of the same plan. The cover test is the one [`Self::run_dns_step`]
    /// removes on — a domain verified through public DNS may sit in a zone no
    /// credential can touch, and the confirm summary must never promise a
    /// removal the run will not make. An unattributed row plans nothing —
    /// attribution never guesses, and a wrong guess here deletes a stranger's
    /// records.
    fn domain_plans(
        &self,
        row: &ManagedServerRow,
        credentials: &[UsableCredential],
    ) -> Vec<DomainPlan> {
        let Some(attributed) = row.domain.as_deref() else {
            return Vec::new();
        };
        let ipv4 = row.ipv4.as_deref();
        let ipv6 = row.ipv6.as_deref();
        let covered = |cover: &Option<ZoneCover>, plan: RetireDnsPlan| {
            if cover.is_some() {
                plan
            } else {
                // Verified, but no credential reaches the zone: the run
                // removes nothing there, so every record that points at this
                // box joins the by-hand list rather than being silently left
                // dangling.
                plan.by_hand_only()
            }
        };
        let mut plans = Vec::with_capacity(1 + row.secondary_domains.len());
        let cover = zone_cover(credentials, attributed);
        plans.push(DomainPlan {
            domain: attributed.to_string(),
            plan: covered(&cover, plan_dns_cleanup(attributed, ipv4, ipv6)),
            cover,
        });
        for secondary in &row.secondary_domains {
            let cover = zone_cover(credentials, secondary);
            plans.push(DomainPlan {
                domain: secondary.clone(),
                plan: covered(
                    &cover,
                    plan_secondary_dns_cleanup(secondary, attributed, ipv4, ipv6),
                ),
                cover,
            });
        }
        plans
    }

    /// The row's whole plan as the confirm summary shows it — every domain's
    /// removals and leftovers, in domain order.
    fn plan_for(&self, row: &ManagedServerRow, credentials: &[UsableCredential]) -> RetireDnsPlan {
        let mut plan = RetireDnsPlan::default();
        for domain_plan in self.domain_plans(row, credentials) {
            plan.append(domain_plan.plan);
        }
        plan
    }

    fn creds_bag(&self) -> Credentials {
        let inner = self.inner.lock().expect("retire mutex");
        Credentials {
            entries: inner
                .creds
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    fn vps(&self) -> Result<VpsDispatch, String> {
        let id = {
            let inner = self.inner.lock().expect("retire mutex");
            inner.provider_id.clone().ok_or("no provider selected")?
        };
        let id = parse_provider_id(&id)?;
        vps_provider(id, self.creds_bag(), self.inputs.provider_base_url.clone())
            .ok_or_else(|| "missing credentials for this provider".to_string())
    }

    /// The DNS dispatcher for one credential, or `None` when that credential
    /// cannot be built (no provider selected, no held credential passed in).
    fn dns(&self, choice: &DnsChoice) -> Option<DnsDispatch> {
        match choice {
            DnsChoice::EnteredToken => {
                let id = self
                    .inner
                    .lock()
                    .expect("retire mutex")
                    .provider_id
                    .clone()?;
                let id = parse_provider_id(&id).ok()?;
                dns_provider(id, self.creds_bag(), self.inputs.provider_base_url.clone())
            }
            DnsChoice::Held(i) => {
                let held = self
                    .inner
                    .lock()
                    .expect("retire mutex")
                    .held_dns
                    .get(*i)?
                    .clone();
                let id = parse_provider_id(&held.provider_id).ok()?;
                let creds = Credentials {
                    entries: held
                        .fields
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                };
                dns_provider(id, creds, self.inputs.provider_base_url.clone())
            }
        }
    }

    fn fail_to_credentials(&self, cause: String) {
        self.with_snap(|s| {
            s.phase = RetirePhase::Credentials;
            s.error = Some(cause);
        });
    }

    /// § Credential stance, in preference order: (a) the entered token when
    /// its provider also has DNS capability and reports zones; (b) the held
    /// `fauna.state.dns` credential the app passed in. **Both** are kept when
    /// both are usable — which one a domain is cleaned through is decided per
    /// domain by [`zone_cover`], on *a zone covering the domain*, so (a)
    /// holding some unrelated zone never shadows (b) holding the right one.
    /// Empty = (c) none: cleanup is not attempted, the by-hand list is the
    /// whole output.
    async fn resolve_dns_credentials(&self) -> Vec<UsableCredential> {
        let held_count = self.inner.lock().expect("retire mutex").held_dns.len();
        let choices =
            std::iter::once(DnsChoice::EnteredToken).chain((0..held_count).map(DnsChoice::Held));
        let mut usable = Vec::with_capacity(1 + held_count);
        for choice in choices {
            if let Some(dns) = self.dns(&choice)
                && let Ok(zones) = dns.verify(&self.http).await
                && !zones.is_empty()
            {
                usable.push(UsableCredential {
                    choice,
                    zones: zones.into_iter().map(|z| (z.name, z.id)).collect(),
                });
            }
        }
        usable
    }

    /// § Box → domain attribution, then § DNS cleanup — several domains: the
    /// attributed domain and every other domain verified to point at the box.
    ///
    /// **Attribution.** The provisioning name is the domain with `.`→`-`, and
    /// `my-site.example.com` and `my.site.example.com` collide, so **the name
    /// alone never attributes**. Candidates are tried in order until one
    /// *verifies* — the domain's apex `A` equals the box's IPv4:
    ///
    /// 1. the box's PTR (provisioning sets it to `mail.<domain>`);
    /// 2. domains the app passed in from local state;
    /// 3. zones the usable DNS credentials report whose dashed form equals
    ///    the server name.
    ///
    /// Unverified → `(None, [])`, and the caller skips DNS cleanup and the
    /// transfer-code affordance for that row. Attribution never guesses.
    ///
    /// **The cleanup set.** Once a domain is attributed, every *other* domain
    /// the box may have served is put through the same verification: the
    /// app-passed candidates again, and **every** zone the usable credentials
    /// report — not only the dashed-name matches, because a secondary domain
    /// is named after nothing about the box. This is a scan that *reads*;
    /// nothing is ever removed on a name, and a zone whose apex points
    /// elsewhere is left entirely alone. Verified ones become the row's
    /// `secondary_domains`.
    async fn verified_domains(
        &self,
        vps: &VpsDispatch,
        server: &ManagedServer,
        credentials: &[UsableCredential],
        apex_cache: &mut BTreeMap<String, Vec<String>>,
    ) -> (Option<String>, Vec<String>) {
        let Some(ipv4) = server.ipv4.as_deref() else {
            return (None, Vec::new());
        };
        let instance: VpsInstance = VpsInstance::from(server);

        let mut candidates: Vec<String> = Vec::new();
        // (1) PTR — `mail.<domain>`, so strip the leading label.
        if let Ok(Some(ptr)) = vps.get_ptr(&self.http, &instance).await {
            let ptr = ptr.trim_end_matches('.');
            let domain = ptr.strip_prefix("mail.").unwrap_or(ptr);
            if !domain.is_empty() {
                candidates.push(domain.to_string());
            }
        }
        // (2) what the app passed in.
        let passed_in = self
            .inner
            .lock()
            .expect("retire mutex")
            .candidate_domains
            .clone();
        let passed_in_for_scan = passed_in.clone();
        candidates.extend(passed_in);
        // (3) zones whose dashed form matches the server name. This is the
        //     collision-prone source, which is exactly why it is still put
        //     through the same verification as the others.
        for (zone_name, _) in credentials.iter().flat_map(|c| c.zones.iter()) {
            if zone_name.replace('.', "-") == server.name {
                candidates.push(zone_name.clone());
            }
        }

        let mut attributed = None;
        for candidate in dedup_in_order(candidates) {
            if self
                .apex_a_points_at(&candidate, ipv4, credentials, apex_cache)
                .await
            {
                attributed = Some(candidate);
                break;
            }
        }
        let Some(attributed) = attributed else {
            return (None, Vec::new());
        };

        // The app-passed half is the live list attribution read above — seeded
        // from the inputs and extended by `add_candidate_domains` — never the
        // page-open snapshot, or a local domain the admin entry learns late
        // attributes but is never cleaned.
        let pool = passed_in_for_scan
            .into_iter()
            .chain(
                credentials
                    .iter()
                    .flat_map(|c| c.zones.iter().map(|(name, _)| name.clone())),
            )
            .filter(|d| *d != attributed);
        let mut secondary = Vec::new();
        for domain in dedup_in_order(pool.collect()) {
            if self
                .apex_a_points_at(&domain, ipv4, credentials, apex_cache)
                .await
            {
                secondary.push(domain);
            }
        }
        (Some(attributed), secondary)
    }

    /// Does `domain`'s apex `A` equal `ipv4`? One read per domain per verify:
    /// the answer does not depend on which box is asking.
    async fn apex_a_points_at(
        &self,
        domain: &str,
        ipv4: &str,
        credentials: &[UsableCredential],
        apex_cache: &mut BTreeMap<String, Vec<String>>,
    ) -> bool {
        if let Some(addresses) = apex_cache.get(domain) {
            return addresses.iter().any(|a| a == ipv4);
        }
        let addresses = self.apex_a_addresses(domain, credentials).await;
        let hit = addresses.iter().any(|a| a == ipv4);
        apex_cache.insert(domain.to_string(), addresses);
        hit
    }

    /// `domain`'s apex `A` values.
    ///
    /// Read through the DNS provider's `find_records` when a credential can
    /// read the domain's zone, **else public DNS from the client** — the same
    /// DoH transport the wizard already uses pre-auth
    /// (`fauna_provisioning::probe`), so a VPS-only provider (four of the six)
    /// or a domain hosted at a DNS provider the person gave no credential for
    /// still attributes.
    ///
    /// The provider holding the zone outranks the public resolver: it is
    /// authoritative and current, where a resolver may serve a cached answer,
    /// so a provider "no" is final. Public DNS is consulted only when the
    /// provider could not be asked at all — no credential, no covering zone,
    /// or the read itself failed. A public read that fails answers nothing,
    /// the safe direction: an unattributed row is still deletable.
    async fn apex_a_addresses(
        &self,
        domain: &str,
        credentials: &[UsableCredential],
    ) -> Vec<String> {
        if let Some(cover) = zone_cover(credentials, domain)
            && let Some(dns) = self.dns(&cover.choice)
        {
            let name = apex_name(&dns, domain, &cover.zones);
            if let Ok(records) = dns
                .find_records(&self.http, &cover.zone_id, &name, "A")
                .await
            {
                return records.into_iter().map(|r| r.value).collect();
            }
        }
        match self.inputs.doh_base_url.as_deref() {
            Some(base) => {
                fauna_provisioning::probe::dns_a_lookup_with_base_url(&self.http, domain, base)
                    .await
            }
            None => fauna_provisioning::probe::dns_a_lookup(&self.http, domain).await,
        }
    }

    fn set_code_state(&self, server_id: &str, state: TransferCodeState) {
        self.with_snap(|s| {
            if let Some(row) = s.servers.iter_mut().find(|r| r.server_id == server_id) {
                row.transfer_code = state;
            }
        });
    }

    /// The run itself, for the **armed** server — never `selected`, which is
    /// the view's to move and so no authority over what gets destroyed.
    async fn run(&self, armed: &str, do_dns: bool) {
        let Some(row) =
            self.with_snap(|s| s.servers.iter().find(|r| r.server_id == armed).cloned())
        else {
            return;
        };
        self.with_snap(|s| {
            s.phase = RetirePhase::Running;
            s.error = None;
            s.force_server_offered = false;
        });

        if do_dns {
            self.with_snap(|s| s.step_mut(RetireStep::Dns).state = StepState::Running);
            match self.run_dns_step(&row).await {
                Ok(state) => self.with_snap(|s| s.step_mut(RetireStep::Dns).state = state),
                Err(cause) => {
                    // Stop before the server. The box stays listed, so the
                    // cleanup remains reachable — that is the whole reason
                    // DNS runs first.
                    self.with_snap(|s| {
                        s.step_mut(RetireStep::Dns).state = StepState::Failed {
                            cause: cause.clone(),
                        };
                        s.error = Some(cause);
                        s.force_server_offered = true;
                    });
                    return;
                }
            }
        } else {
            self.with_snap(|s| {
                let dns = s.step_mut(RetireStep::Dns);
                if !matches!(dns.state, StepState::Done) {
                    dns.state = StepState::Skipped {
                        why: "dns_step_forced_past".into(),
                    };
                }
                // Every plan line the failed step never removed is now the
                // person's to delete by hand — and it still points at the
                // box, so it carries that urgency, not the stale-TXT one.
                let not_removed: Vec<LeftoverLine> = s
                    .dns_plan
                    .iter()
                    .filter(|l| !l.removed)
                    .map(|l| LeftoverLine {
                        name: l.name.clone(),
                        record_type: l.record_type.clone(),
                        value: l.value.clone(),
                        kind: LeftoverKind::PointsAtBox,
                    })
                    .filter(|l| !s.leftover_records.contains(l))
                    .collect();
                // Ahead of the shared-name `TXT`, as the no-credential list
                // orders them.
                s.leftover_records.splice(0..0, not_removed);
            });
        }

        self.with_snap(|s| s.step_mut(RetireStep::Server).state = StepState::Running);
        let vps = match self.vps() {
            Ok(v) => v,
            Err(e) => return self.fail_server_step(e),
        };
        let raw = self
            .inner
            .lock()
            .expect("retire mutex")
            .raw
            .iter()
            .find(|s| s.server_id == row.server_id)
            .map(VpsInstance::from);
        let instance = raw.unwrap_or(VpsInstance {
            server_id: row.server_id.clone(),
            ipv4: row.ipv4.clone().unwrap_or_default(),
        });

        // `delete_server` treats a 404 as success: a box already gone has
        // achieved the goal, and a re-run after a crash must not raise.
        match vps.delete_server(&self.http, &instance).await {
            Ok(()) => self.with_snap(|s| {
                s.step_mut(RetireStep::Server).state = StepState::Done;
                s.phase = RetirePhase::Done;
                s.servers.retain(|r| r.server_id != row.server_id);
                s.retired = Some(row.clone());
            }),
            Err(e) => self.fail_server_step(format!("{e}")),
        }
    }

    fn fail_server_step(&self, cause: String) {
        self.with_snap(|s| {
            s.step_mut(RetireStep::Server).state = StepState::Failed {
                cause: cause.clone(),
            };
            s.error = Some(cause);
        });
    }

    /// Remove every planned record whose value actually points at this box,
    /// domain by domain, each through the credential whose zone covers it.
    /// Each removal is a `find_records` pre-flight followed by a
    /// `delete_record` for the matching entries, so a record another service
    /// put at the same name survives — the scope rule is *value-scoped, never
    /// a name sweep*. The plans are the ones the confirm summary showed
    /// ([`Self::domain_plans`]), so the run never does more than it promised.
    async fn run_dns_step(&self, row: &ManagedServerRow) -> Result<StepState, String> {
        if row.domain.is_none() {
            return Ok(StepState::Skipped {
                why: "no_verified_domain".into(),
            });
        }
        let credentials = self.inner.lock().expect("retire mutex").credentials.clone();
        if credentials.is_empty() {
            return Ok(StepState::Skipped {
                why: "no_dns_credential".into(),
            });
        }
        let covered: Vec<(DomainPlan, ZoneCover)> = self
            .domain_plans(row, &credentials)
            .into_iter()
            .filter_map(|p| p.cover.clone().map(|cover| (p, cover)))
            .collect();
        if covered.is_empty() {
            return Ok(StepState::Skipped {
                why: "no_zone_for_domain".into(),
            });
        }

        for (domain_plan, cover) in &covered {
            let dns = self
                .dns(&cover.choice)
                .ok_or_else(|| "dns credential no longer available".to_string())?;
            for removal in &domain_plan.plan.removals {
                self.remove_one(
                    &dns,
                    &cover.zone_id,
                    &domain_plan.domain,
                    &cover.zones,
                    removal,
                )
                .await?;
                // Recorded per line as the walk goes, so a step that fails
                // part way leaves an honest account of what is still there.
                self.with_snap(|s| {
                    for line in s.dns_plan.iter_mut().filter(|l| {
                        l.name == removal.name
                            && l.record_type == removal.record_type
                            && l.value == removal.value
                    }) {
                        line.removed = true;
                    }
                });
            }
        }
        Ok(StepState::Done)
    }

    async fn remove_one(
        &self,
        dns: &DnsDispatch,
        zone_id: &str,
        domain: &str,
        zones: &[(String, String)],
        removal: &PlannedRemoval,
    ) -> Result<(), String> {
        let name = relative_name(dns, &removal.name, domain, zones);
        let found = dns
            .find_records(&self.http, zone_id, &name, &removal.record_type)
            .await
            .map_err(|e| format!("{e}"))?;
        for record in found {
            // An empty planned value means "the whole rrset at this name" —
            // only the floor TLSA, whose value we cannot reconstruct.
            if !removal.value.is_empty() && !value_matches(&record.value, &removal.value) {
                continue;
            }
            dns.delete_record(
                &self.http,
                zone_id,
                &name,
                &removal.record_type,
                &record.value,
            )
            .await
            .map_err(|e| format!("{e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
