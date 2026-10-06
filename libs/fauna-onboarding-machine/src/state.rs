use std::collections::HashMap;

use fauna_core::secret::SecretString;
use fauna_provisioning::dns::DnsZone;
use fauna_provisioning::registrar::{ContactInfo, RegistrarAvailability};
use fauna_provisioning::vps::{ServerTypeInfo, VpsLocation};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[derive(Default)]
pub enum OnboardingStep {
    #[default]
    IdentityChoice,
    IdentityCreated,
    IdentityImport,
    /// The recovery-kit offer, right after the identity secret
    /// (`onboarding.md` § 1 Identity; position is a user ruling, 2026-07-23).
    /// The screen **mints and displays only** — no nest exists at this point,
    /// so registration + escrow run at the wizard's signed-in handoff
    /// (`identity-succession.md` § The RecoveryKey → *Creation UX*, ratified
    /// 2026-08-01). Entered from `confirm_generated_identity` **only when the
    /// app declared `set_renders_recovery_kit(true)`** — apps whose screen
    /// hasn't landed keep going straight to `HandleEntry`, a per-app parity
    /// gap, not drift. Confirm and skip both land on `HandleEntry`.
    RecoveryKit,
    /// The phrase-only identity restore (`onboarding.md` § 1 Identity),
    /// reached from `identity_choice` via `restore-from-recovery-kit-button` —
    /// so only apps that render that button ever enter it; no capability flag
    /// is needed. On success the identity seed is restored and the wizard
    /// lands on `HandleEntry`, uniform with an import. Distinct from
    /// [`BoxRecoveryEntry`], which names the total-box-loss NEST recovery.
    RecoveryEntry,
    HandleEntry,
    DnsConfig,
    VpsConfig,
    NestProvisioning,
    DnsPostInstructions,
    InviteRequest,
    /// Reached only when handle-check returns
    /// `HandleCheckOutcome::UnregisteredUnclaimedNest` — the nest is
    /// running but `setup-status.claimed == false`. Mutually exclusive
    /// with `InviteRequest`: there's no admin to issue invites yet, so
    /// the user's only path forward is the one-time claim code printed
    /// by the nest's bootstrap process. See
    /// `docs/goal/behavior/onboarding.md` §3a.
    ClaimCode,
    /// The `nat_mode_choice` page — the **single, terminal** admin-path setup
    /// step: the admin confirms the nest's NAT axis (`public` / `private`).
    /// Reached **directly on claim completion** — there is no storage-mode
    /// question, because there is no storage mode (`storage-modes.md`; a nest
    /// is sealed and content-ready from first boot).
    /// `submit_nat_mode_choice` commits via the mutable `fauna.setup.nat_mode`
    /// kind and exits to `Done` with `wizard_outcome() == LoggedIn`;
    /// `defer_nat_mode_choice` exits the same way keeping the seeded value. Per
    /// `docs/goal/behavior/onboarding.md` § 3b-bis.
    NatModeChoice,
    /// The one-tap "trust this box" offer (`onboarding.md` § 3b-ter; ui.yaml
    /// page `onboarding.trust_prompt`) — an **optional interstitial before
    /// LoggedIn, not a setup step**: it configures nothing nest-side, so
    /// [`Self::NatModeChoice`] stays the terminal admin-path *setup* step.
    ///
    /// The screen **asks only.** Minting the default grant set needs an
    /// authenticated session and the nest's content-processor roster, neither
    /// of which the wizard holds, so the answer is latched
    /// ([`crate::OnboardingMachine::take_trust_prompt_granted`]) and the mint
    /// runs at the signed-in handoff — the same deferral
    /// [`Self::RecoveryKit`] uses for registration + escrow. Entered from
    /// every route that *establishes* the user on the nest — the NAT step's
    /// two exits on the claim path, plus an invite redemption and an approved
    /// join request (§ 3b-ter's "joining user's first login") — and **only
    /// when the app declared `set_renders_trust_prompt(true)`**; apps whose
    /// screen hasn't landed keep going straight to [`Self::Done`], a per-app
    /// parity gap, not drift. A returning `AlreadyOnNest` sign-in is NOT such
    /// a route and never reaches here. Grant and skip both land on
    /// [`Self::Done`] with the same `LoggedIn` outcome.
    TrustPrompt,
    /// Total-box-loss recovery hub (`box-recovery.md` § Recovery UI, step 4;
    /// ui.yaml page `nest_recovery`). Lists the admin's custodied boxes
    /// (`fauna.state.deployment-seeds`, read via the shared `deploymentSeeds()`
    /// getter — this device's store joined with a reachable nest's); the admin selects one and picks a re-provision method (cloud vs
    /// self-hosted). Reached from two entries: `launch-recover-button` on
    /// `launch_retry` (surviving device) and `recover-lost-box-button` on
    /// `identity_choice` → `identity_import` (recovery intent) → here.
    NestRecovery,
    /// Self-hosted recovery instructions (`box-recovery.md` § Recovery UI,
    /// step 4; ui.yaml page `recover_selfhosted_instructions`). Shows the
    /// installer invocation / `.env` line carrying `FAUNA_DEPLOYMENT_SEED` (the
    /// client-custodied seed for the selected box) so `docker compose up` boots
    /// the box with the same `nest_actor_id`. An out-of-band step — the box
    /// isn't up yet, so continuing exits the wizard.
    RecoverSelfhostedInstructions,
    /// Sentinel: the wizard finished. App router should swap to the
    /// authenticated UI. Caller may also query `wizard_outcome()` for the
    /// terminal `WizardOutcome` and route to the main app.
    Done,
}

/// Which entry the total-box-loss recovery wizard branch was reached from
/// (`box-recovery.md` § Recovery UI, step 4). Determines where
/// `recover-back-button` returns to and lets the per-app glue own the
/// launch↔wizard boundary the machine can't represent (`launch_retry` is a
/// launch-flow surface, not an `OnboardingStep`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum BoxRecoveryEntry {
    /// `launch-recover-button` on `launch_retry` (surviving device). Back exits
    /// the wizard to the launch retry surface — glue-owned.
    Launch,
    /// `recover-lost-box-button` on `identity_choice` → `identity_import`. Back
    /// returns to `identity_import`.
    Identity,
}

/// The nest's NAT mode (`Public` / `Private`) — the network-reachability
/// axis the admin confirms on the `nat_mode_choice` page. Canonical
/// definition lives in `fauna-core` (`fauna_core::nat_mode::NodeMode`) —
/// one enum, one home, shared with the nest (`fauna_nest::config::NodeMode`
/// re-exports the same type) and the admin client; re-exported here for the
/// `crate::state` / `crate::nest_api` call sites. Per
/// `docs/goal/behavior/onboarding.md` § 3b-bis.
pub use fauna_core::nat_mode::NodeMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum IdentityOrigin {
    Created,
    Imported,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsConfigState {
    pub buy_domain: bool,
    pub same_provider_for_vps: bool,
    pub set_up_later: bool,
    pub selected_provider_id: Option<String>,
    /// Provider field-id → secret value, captured from the wizard's credential
    /// form. The values are [`SecretString`] (zeroized on drop, redacted Debug)
    /// so the API token does not linger as a plain `String` for the whole DNS
    /// step; serde is transparent (a text string) so the WASM JSON / uniffi
    /// marshalling are byte-identical to a plain `String` map.
    pub creds: HashMap<String, SecretString>,
    pub verified: bool,
    pub zone_id: Option<String>,
    /// All zones the registrar reports for the connected account, populated
    /// by `verify_dns()`. Used by `provider_status()` to detect the
    /// `ProviderHasDomain` case (zone matching `handle_domain()` exists).
    pub current_zones: Vec<DnsZone>,
    /// Per-domain availability returned by `Registrar::availability()` when
    /// `verify_dns` runs against an unregistered domain on a registrar-
    /// capable provider. None means the call wasn't attempted (either no
    /// registrar capability or domain isn't `Unregistered`) or it returned
    /// `Unavailable` / `TldNotSupported` (treated as "not buyable" for
    /// `provider_status()` purposes — see `UnregisteredNotBuyable`).
    pub current_availability: Option<RegistrarAvailability>,
    /// WHOIS contact for the buy-domain path. Pre-populated by
    /// `verify_dns` via `Registrar::fetch_default_contact()` when the
    /// provider's `requires_contact()` is true; user confirms/edits via
    /// `set_contact()`. Passed to `provision_with_registration` from
    /// `start_provisioning`.
    pub contact: Option<ContactInfo>,
    /// Set by `confirm_price()` after the wizard surfaces the registrar's
    /// quoted price to the user. `start_provisioning` refuses to advance
    /// through the buy-domain path until this is true (so we never charge
    /// the user without explicit consent).
    pub price_agreed: bool,
    /// True while `verify_dns()` is in flight. Set to true at the top of
    /// the call, cleared on every return path (success or error). The
    /// `can_verify_dns()` predicate checks this so the wizard's verify
    /// button stays disabled across all 7 apps without per-app
    /// in-flight tracking. Not serialized — transient runtime state.
    #[serde(skip)]
    pub verifying: bool,
    /// Per `hosted-auth` field id: where its device-authorization sign-in
    /// stands (`OnboardingMachine::hosted_auth_state`; absent = `Idle`). The
    /// app paints the field's button label from this
    /// (`docs/goal/behavior/onboarding.md` § 4); the token itself lands in
    /// [`creds`](Self::creds) under the same field id.
    #[serde(default)]
    pub hosted_auth: HashMap<String, HostedAuthState>,
}

/// The DNS-provider credential the admin verified during onboarding's DNS
/// step, exposed by [`crate::OnboardingMachine::captured_dns_credential`] for
/// the **launched client** to seal into `fauna.state.dns` via the
/// post-onboarding `DnsManagementMachine::PutCredentials` path — one store,
/// one writer (`docs/goal/behavior/dns-management.md` § Where the credential
/// lives; `docs/goal/behavior/onboarding.md` § 4). The onboarding machine has
/// no account-plane write capability and, in the fresh-provision path, no live
/// nest "at the end of the DNS step", so it does not seal here; it hands the
/// captured credential to the launched client (the onboarding→launch hand-off
/// channel).
///
/// The shape mirrors the `PutCredentials` inputs: `fields` is the provider
/// field-id → value bag (the same `DnsConfigState.creds` map, keyed by
/// `providers.yaml` field ids). The covered zones are **not** carried — the
/// machine re-runs `verify()` to (re)derive them at seal time, so what
/// onboarding captured cannot go stale. The secret field values live only in
/// this in-process value and the sealed `fauna.state.dns` row; they are never
/// sent to the nest in plaintext (account-plane rows rest client-sealed,
/// nest-opaque).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CapturedDnsCredential {
    /// `fauna_provisioning::ProviderId::as_str()` (e.g. `"hetzner"`).
    pub provider_id: String,
    /// Provider field-id → secret value, keyed by `providers.yaml` field ids.
    /// Maps to `DnsAction::PutCredentials.fields` (`Vec<DnsCredentialField>`,
    /// whose `value` is the same [`SecretString`]) in the per-app launch glue
    /// with no transformation beyond the map→vec shape — the secret stays
    /// `SecretString` end-to-end into the downstream `fauna.state.dns` store.
    pub fields: HashMap<String, SecretString>,
    /// Default user-facing label, `"{provider_id} ({domain})"` — disambiguates
    /// multiple held credentials; the credential UI can rename it later.
    pub label: String,
}

/// Per-provider DNS-config status surfaced to the wizard's UI. Computed
/// by `OnboardingMachine::provider_status()` from already-in-state
/// inputs; pure function, no IO.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ProviderStatus {
    /// No provider selected, or `verify_dns` hasn't completed yet.
    /// UI: hide status text. Continue disabled.
    NotReady,
    /// Provider's DNS account already hosts this handle's zone.
    /// UI: "you own this domain at <provider>". Continue enabled.
    ProviderHasDomain,
    /// Domain is registered, zone NOT in this provider's zone list.
    /// UI: "transfer this domain to <provider> manually first."
    /// Continue disabled.
    RegisteredElsewhere,
    /// Domain unregistered AND `Registrar::availability()` returned
    /// `Buyable`. UI shows price + price-confirm checkbox + (per
    /// `requires_contact()`) the WHOIS form. Continue enabled iff
    /// `price_agreed && (!requires_contact || contact.is_some())`.
    UnregisteredBuyable {
        price_cents: u64,
        currency: Option<String>,
    },
    /// Domain unregistered AND no buyable signal — provider has no
    /// Registrar capability, or `availability()` returned `Unavailable`
    /// / `TldNotSupported`. UI: "<provider> can't sell this — pick
    /// another or buy elsewhere."
    UnregisteredNotBuyable,
}

/// One priced line item on the `nest_provisioning` page's top-region price
/// summary ("Bill of Materials" — `docs/goal/behavior/onboarding.md` §6).
/// `label` reuses the same
/// `onboarding.provision.step.{domain,server}` keys the progress rows below
/// it already render, so the recap and the step name always agree — see
/// [`crate::OnboardingMachine::bill_of_materials`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BillOfMaterialsItem {
    pub label: LocalizedText,
    pub price_cents: u64,
    pub currency: String,
    /// `true` for the VPS's per-month charge, `false` for the domain's
    /// one-time registration charge.
    pub recurring: bool,
    /// The domain line only: the registrar's per-year renewal price in
    /// `currency`, when it quoted one (`RegistrarAvailability::Buyable.
    /// renewal_cents`) — rendered as `onboarding.provision.bom_line_domain`
    /// ("{price} for the first year, then {renewal}/year"), else the plain
    /// `bom_line`. Always `None` on the VPS line. `onboarding.md` § 6.
    #[serde(default)]
    pub renewal_price_cents: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct VpsConfigState {
    pub selected_provider_id: Option<String>,
    /// Provider field-id → secret value (the VPS-provider API token/keys). Same
    /// [`SecretString`] discipline as [`DnsConfigState::creds`].
    pub creds: HashMap<String, SecretString>,
    pub verified: bool,
    pub server_types: Vec<ServerTypeInfo>,
    pub selected_server_type_id: Option<String>,
    pub locations: Vec<VpsLocation>,
    pub selected_location_id: Option<String>,
    /// Mail-vs-social intent for the box, set by the `vps-config-mail-mode-toggle`.
    /// `None` = "not chosen — use the handle's real-domain default"
    /// (`OnboardingMachine::provision_mail_mode_enabled`); `Some(true)` = mail box
    /// (scanner sidecars + mail ports, needs `mem_gb ≥ 2`), `Some(false)` =
    /// social-only box (lean nest+watchtower compose, the 1 GB tier). Feeds
    /// `CloudInitParams::enable_mail` via `run_provisioning_inner`.
    /// `docs/goal/behavior/onboarding.md` §5.
    #[serde(default)]
    pub enable_mail: Option<bool>,
    /// Which builds the box's updater follows, set by the
    /// `vps-config-update-channel-row` radios. `None` = "not chosen — the
    /// default channel" (`OnboardingMachine::provision_update_channel`). Feeds
    /// `CloudInitParams::image_tag` via `run_provisioning_inner`.
    /// `docs/goal/behavior/onboarding-provisioning.md` §5.
    #[serde(default)]
    pub update_channel: Option<fauna_provisioning::cloud_init::UpdateChannel>,
    /// True while `verify_vps()` is in flight. See the matching field on
    /// `DnsConfigState` for rationale. Not serialized.
    #[serde(skip)]
    pub verifying: bool,
    /// Per `hosted-auth` field id: its sign-in state on the VPS form — the
    /// twin of [`DnsConfigState::hosted_auth`], mirrored from it by
    /// `continue_from_dns` when the same provider serves both.
    #[serde(default)]
    pub hosted_auth: HashMap<String, HostedAuthState>,
}

/// Which credential form a `hosted-auth` field is being driven on — the two
/// forms hold separate credential bags (`DnsConfigState::creds` /
/// `VpsConfigState::creds`), so the sign-in names which one its token lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum CredentialForm {
    Dns,
    Vps,
}

/// Where a `hosted-auth` field's sign-in stands. Drives the field's button
/// label on every app (`docs/goal/behavior/onboarding.md` § 4):
/// `Idle` → "Sign in at the provider…", `Pending` → "Finish signing in in your
/// browser — code {user_code}", `Connected`, `Failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum HostedAuthState {
    Idle,
    /// The device-authorization request is out; the user is (or should be)
    /// on the provider's hosted page. `verification_url` is what the app
    /// opened, `user_code` the fallback the user can type there.
    Pending {
        user_code: String,
        verification_url: String,
    },
    /// The token landed in the form's credential bag under the field id.
    Connected,
    /// The attempt ended without a token (declined, expired, or a transport
    /// error) — the button offers to start over.
    Failed {
        message: String,
    },
}

/// What `OnboardingMachine::hosted_auth_begin` hands the app: the URL to open
/// through its existing open-URL affordance and the code to show beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct HostedAuthPrompt {
    pub verification_url: String,
    pub user_code: String,
}

/// i18n-aware text carrier returned by helpers like `dns_status_text_key()`
/// and `format_price_key()`. Canonical definition lives in `fauna-core`
/// (`fauna_core::localized::LocalizedText`) so every shared-Rust machine —
/// onboarding, the folder wizard, … — shares one `uniffi::Record` instead of
/// each minting its own (which would name-collide in the generated bindings).
/// Re-exported here for the `crate::state` / `crate::machine` call sites and
/// the public `fauna_onboarding_machine::LocalizedText` surface. The
/// English-rendered helpers (`format_price`, `dns_status_text`) remain
/// available for clients that don't localize.
pub use fauna_core::localized::LocalizedText;

/// The app's age claim for the admission it is about to make — the machine
/// surface of `fauna_protocol::age::AgeClaim` (`family-safety.md` § The
/// account age band). Set by the mobile apps from their store age signal
/// (`OnboardingMachine::set_age_claim`); every other app never sets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AgeClaimPlain {
    /// Wire token: `U13` | `13-15` | `16-17` | `18+`.
    pub band: String,
    /// The platform attestation hardening the claim; `None` = declared-only.
    pub attestation: Option<AgeAttestationPlain>,
}

/// The platform attestation over `{nest nonce, band, application id, actor}`
/// — mirror of `fauna_protocol::age::AgeAttestation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AgeAttestationPlain {
    /// `"ios"` | `"android"`.
    pub platform: String,
    /// The `request_age_nonce` hex the attestation was made over.
    pub nonce_hex: String,
    /// iOS: the App Attest key id (hex); android: `""`.
    pub key_id_hex: String,
    /// iOS: the App Attest attestation object; android: the Play Integrity
    /// classic-request verdict token (ASCII bytes).
    pub attestation_object: Vec<u8>,
}

/// `request_age_nonce`'s reply — mirror of `nest_api::AgeNonce`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AgeNoncePlain {
    /// 64-char hex of the 32-byte nonce the attestation must bind.
    pub nonce_hex: String,
    /// Seconds the nonce stays redeemable — the attestation round's budget.
    pub expires_in_secs: u64,
    /// The `AgeAttestationPlain::platform` tokens this nest can verify. The
    /// machine sends an attestation only for a platform listed here, over
    /// this nonce, to this nest (`family-safety.md` § The account age band →
    /// *An attestation the nest cannot check*); the glue may read it to skip
    /// a platform round that would be discarded — an economy, never the
    /// guarantee. Empty = verifies nothing (an unarmed nest holds no verifier).
    #[cfg_attr(feature = "uniffi", uniffi(default = []))]
    pub attestation_platforms: Vec<String>,
}

impl From<AgeClaimPlain> for fauna_protocol::age::AgeClaim {
    fn from(c: AgeClaimPlain) -> Self {
        Self {
            band: c.band,
            attestation: c.attestation.map(|a| fauna_protocol::age::AgeAttestation {
                platform: a.platform,
                nonce: a.nonce_hex,
                key_id: a.key_id_hex,
                attestation_object: fauna_protocol::ByteBuf::from(a.attestation_object),
                extra: Default::default(),
            }),
            extra: Default::default(),
        }
    }
}

impl From<&fauna_protocol::age::AgeClaim> for AgeClaimPlain {
    fn from(c: &fauna_protocol::age::AgeClaim) -> Self {
        Self {
            band: c.band.clone(),
            attestation: c.attestation.as_ref().map(|a| AgeAttestationPlain {
                platform: a.platform.clone(),
                nonce_hex: a.nonce.clone(),
                key_id_hex: a.key_id.clone(),
                attestation_object: a.attestation_object.to_vec(),
            }),
        }
    }
}

/// Plain mirror of `fauna_provisioning::dns::DnsRecord` for FFI return from
/// `fetch_dkim()`. The native orchestrator type lives in fauna-provisioning;
/// this is the same shape but lives in the machine crate so client bindings
/// see it as part of the machine's surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DnsRecordPlain {
    pub record_type: String,
    pub name: String,
    pub value: String,
    pub ttl: u32,
    pub priority: Option<u32>,
}

impl From<&fauna_provisioning::dns::DnsRecord> for DnsRecordPlain {
    fn from(r: &fauna_provisioning::dns::DnsRecord) -> Self {
        Self {
            record_type: r.record_type.clone(),
            name: r.name.clone(),
            value: r.value.clone(),
            ttl: r.ttl,
            priority: r.priority,
        }
    }
}

/// FFI-friendly mirror of `fauna_provisioning::providers_generated::FieldMeta`.
/// The generated FieldMeta uses `&'static str` which UniFFI can't carry across
/// the boundary. This struct is what client bindings see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct FieldMetaPlain {
    pub id: String,
    pub field_type: FieldTypePlain,
    pub label_key: String,
    pub required: bool,
    pub kinds: Vec<CapabilityPlain>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum FieldTypePlain {
    Text,
    Secret,
    Select,
    /// A hosted sign-in button, not an input: the value is the Bearer token
    /// the provider's device-authorization flow yields
    /// (`OnboardingMachine::hosted_auth_begin` / `hosted_auth_wait`;
    /// `docs/goal/behavior/onboarding.md` § 4). An app that has not built
    /// the button renders it as `Secret` — a pasted token is equally valid.
    HostedAuth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum CapabilityPlain {
    Dns,
    Vps,
    Registrar,
}

impl From<&fauna_provisioning::FieldMeta> for FieldMetaPlain {
    fn from(f: &fauna_provisioning::FieldMeta) -> Self {
        Self {
            id: f.id.to_string(),
            field_type: match f.field_type {
                fauna_provisioning::FieldType::Text => FieldTypePlain::Text,
                fauna_provisioning::FieldType::Secret => FieldTypePlain::Secret,
                fauna_provisioning::FieldType::Select => FieldTypePlain::Select,
                fauna_provisioning::FieldType::HostedAuth => FieldTypePlain::HostedAuth,
            },
            label_key: f.label_key.to_string(),
            required: f.required,
            kinds: f
                .kinds
                .iter()
                .map(|c| match c {
                    fauna_provisioning::Capability::Dns => CapabilityPlain::Dns,
                    fauna_provisioning::Capability::Vps => CapabilityPlain::Vps,
                    fauna_provisioning::Capability::Registrar => CapabilityPlain::Registrar,
                })
                .collect(),
        }
    }
}

/// Internal — the full wizard state. In-memory only; not persisted.
/// Identity-confirmation methods return the secret hex so the per-app
/// glue can write it to its long-term store immediately. Not exposed via
/// UniFFI; clients see snapshots through individual getters.
#[derive(Debug, Clone, Default)]
pub(crate) struct State {
    pub step: OnboardingStep,
    pub identity_origin: Option<IdentityOrigin>,
    /// The freshly-minted root identity seed (64-char hex), set by
    /// `begin_create_identity`. Held as [`SecretString`] (zeroize-on-drop +
    /// redacted `Debug`) — this is the whole account: whoever reads it owns
    /// the identity outright. Closes the `key-material-hierarchy.md` §
    /// Carrier shape residual this wizard's own in-flight copy left open
    /// after the account-registry read family (`SessionMaterial`/
    /// `StoredAccount`) was closed. Public getters
    /// ([`crate::OnboardingMachine::generated_secret`],
    /// [`crate::OnboardingMachine::effective_secret`]) still return
    /// `Option<String>` — the redaction is crate-internal.
    pub generated_secret: Option<SecretString>,
    /// The user-supplied or escrow-restored root identity seed (64-char
    /// hex). Same custody rule and rationale as [`Self::generated_secret`] —
    /// same carrier family, both fields moved together in one commit.
    pub imported_secret: Option<SecretString>,
    /// Predecessor identity seeds a phrase-only restore recovered from the
    /// escrow blob's additive section — the corpus re-seal window's backstop
    /// (`identity-succession.md` § Seed escrow). Empty in every ordinary
    /// onboarding. Parked here for the client to persist into its account
    /// registry, exactly as `imported_secret` is: this machine owns no store.
    pub restored_predecessors: Vec<crate::nest_api::RestoredPredecessorSeed>,
    /// Set when that section was present but did not open. Distinct from an
    /// empty `restored_predecessors` — see
    /// [`RecoveryEntryOutcome::RestoredPredecessorsLost`](crate::RecoveryEntryOutcome::RestoredPredecessorsLost).
    pub restored_predecessors_unreadable: Option<String>,
    pub handle: String,
    pub local_nest_reachable: bool,
    pub nest_url: String,
    /// The zone the handle's domain sits inside when the handle check found
    /// it undelegated (`example.com` for `dev.example.com`;
    /// `fauna_provisioning::probe::NsProbe::enclosing_zone`). Set alongside a
    /// `DomainAvailable` outcome; `submit_handle_check_continue` lands
    /// dns_config with `buy_domain` off when it is `Some`
    /// (`onboarding-provisioning.md` § 4).
    pub handle_enclosing_zone: Option<String>,
    pub dns: DnsConfigState,
    pub vps: VpsConfigState,
    /// Records to surface on the manual-DNS-setup page; populated when the
    /// orchestrator returns from the `provision_nest_no_dns` deferred path.
    /// Empty otherwise. Mirror of `fauna_provisioning::dns::DnsRecord` in
    /// FFI-friendly shape via `DnsRecordPlain`.
    pub dns_records: Vec<DnsRecordPlain>,
    pub error_message: Option<String>,
    pub is_loading: bool,
    /// The target nest's resolved NAT-mode seed (`node_mode`), as learned from
    /// the setup-status probe. `None` until the probe
    /// resolves. Read by `reset_nat_mode_snapshot` to pre-select the
    /// `nat_mode_choice` page (confirm-only in the common case) — a `Private`
    /// seed always pre-selects Private; a `Public`/absent seed is refined
    /// private-ward when the handle targets a private network. Per
    /// `docs/goal/behavior/onboarding.md` § 3b-bis Defaulting note.
    pub node_mode_seed: Option<NodeMode>,
    /// A claim code the client already holds and wants the `ClaimCode` page to
    /// pre-fill its input with. Set by
    /// `navigate_to_claim_code_for_known_nest_with_code` — the factory-reset
    /// re-onboard path, where `fauna.admin.factory_reset` returned the new code
    /// to the client (the human never sees it, so without pre-fill they'd be
    /// stranded). `None` for the ordinary unclaimed-nest claim path, where the
    /// human types the code the admin printed. The page reads it once (when
    /// the input is empty) and the value is otherwise UI-side state.
    pub claim_code_prefill: Option<String>,
    /// True once **this wizard run** has completed an admin claim of the box —
    /// the claim axis of the § 3b serving-enablement derivation
    /// ([`crate::OnboardingMachine::email_enable_requested`] + siblings).
    ///
    /// § 3b is "Serving enablement **at claim**", but its two published axes
    /// (handle locality, NAT) describe only how the intents *default* once a
    /// claim has happened — neither asks whether one did. The wizard reaches
    /// `WizardOutcome::LoggedIn` from three routes, and only the claim routes
    /// may request enablement: an `AlreadyOnNest` **sign-in** and the
    /// invite-redeem path both land on the very same outcome the per-app launch
    /// glue reads those getters at, so without this flag a returning admin
    /// merely signing in fired four Admin-class deployment writes against a box
    /// they had already configured (found against the live box).
    ///
    /// Set at **both** claim-success sites — `wizard_submit_claim_code` and the
    /// manual-DNS `recheck_manual_dns` → `complete_manual_dns_claim` — because
    /// only one of them moves the claim-code snapshot to `Claimed`, so that
    /// snapshot is not a sound discriminator. Cleared by
    /// [`crate::OnboardingMachine::reset`] with the rest of the run.
    pub claim_completed: bool,
    /// True while the wizard is in the total-box-loss recovery branch
    /// (`box-recovery.md` § Recovery UI, step 4). Set by `begin_recover_lost_box`
    /// / `seed_identity_for_recovery`; routes the `identity_import` success
    /// transition to `NestRecovery` instead of `HandleEntry`, and marks the
    /// downstream provisioning as recovery-mode (re-install the custodied seed
    /// rather than mint a fresh one). Cleared when the wizard backs out to
    /// `IdentityChoice`.
    pub recovery_intent: bool,
    /// Which entry the recovery branch was reached from — decides where
    /// `recover-back-button` returns to (`came-from-launch` vs `came-from-identity`
    /// in ui.yaml). `None` outside the recovery branch.
    pub recovery_came_from: Option<BoxRecoveryEntry>,
    /// App capability declaration: this app has the `recovery_kit` onboarding
    /// screen built, so `confirm_generated_identity` routes through it.
    /// Default `false` — the six apps whose screen hasn't landed keep the
    /// pre-existing straight-to-`HandleEntry` flow untouched (batched
    /// trickle-down flips this per app; when all seven declare it, delete the
    /// flag and hard-code the transition). The machine owns the routing either
    /// way — the app states only the fact (priority #2).
    ///
    /// **Survives [`crate::OnboardingMachine::reset`]** — and so must any
    /// capability flag added beside it. The app declares this once when it
    /// builds the machine and nothing re-declares it, so a reset that cleared
    /// it would route onboarding around a screen the app *does* render for the
    /// rest of that process's life (pinned by
    /// `recovery_kit_navigation::a_factory_reset_preserves_the_apps_declared_capability`).
    pub renders_recovery_kit: bool,
    /// App capability: this app renders the `trust_prompt` interstitial
    /// (`onboarding.md` § 3b-ter). Same contract as `renders_recovery_kit`
    /// above, including surviving [`crate::OnboardingMachine::reset`] —
    /// default `false` keeps every app that hasn't built the screen exiting
    /// the NAT step straight to `Done`.
    pub renders_trust_prompt: bool,
    /// The `trust_prompt` answer, latched for the wizard's signed-in handoff
    /// (`onboarding.md` § 3b-ter). `true` only between
    /// `grant_default_trust()` and the handoff's consume-once
    /// [`crate::OnboardingMachine::take_trust_prompt_granted`]; skipping never
    /// sets it. Deliberately a bare bool and not the grant itself: the wizard
    /// holds no authenticated session, so what it can carry across the handoff
    /// is the *answer*, never the capability.
    pub trust_prompt_granted: bool,
    /// The `(nest_url, handle)` a parked `trust_prompt` will conclude the
    /// wizard with — captured when the arriving route reached it, never
    /// re-derived when the user answers (`onboarding.md` § 3b-ter).
    ///
    /// The four routes that reach `LoggedIn` do not agree on how the pair is
    /// built: a claim takes the persisted-else-effective URL, an invite
    /// redemption concludes with `effective_nest_url()` (which a provider
    /// override redirects), the approved-request poll deliberately uses the
    /// persisted `state.nest_url` *instead* of the override, and an
    /// `AlreadyOnNest` sign-in resolves the domain the user typed. So the
    /// interstitial has to carry the answering route's own pair across the
    /// park — a finisher that recomputed would hand some routes a different
    /// nest than the one they actually joined, and that value is the one the
    /// identity store persists forever. `None` whenever no offer is parked.
    /// Progress, not capability: cleared by [`crate::OnboardingMachine::reset`]
    /// with the rest of `State`. Pinned by
    /// `trust_prompt_navigation::the_offer_concludes_with_the_nest_the_route_actually_joined`.
    pub trust_prompt_pending_outcome: Option<(String, String)>,
    /// The RecoveryKey root minted for the `recovery_kit` onboarding screen —
    /// 64-hex, displayed once, **unregistered until the wizard's signed-in
    /// handoff** (`identity-succession.md` § The RecoveryKey → *Creation UX*).
    /// The onboarding→handoff hand-off latch:
    /// held as [`SecretString`] (zeroize-on-drop + redacted `Debug`) and never
    /// persisted anywhere — offline-only custody means a wizard exit that
    /// never signs in loses it, and Settings' never-created warning then
    /// tells the truth. Minted on first entry to `RecoveryKit`; **reused** on
    /// re-entry after back-navigation (a re-mint would silently invalidate a
    /// phrase the user already wrote down); dropped by `skip_recovery_kit`.
    /// Never-persisted is compiler-enforced: `State`
    /// deliberately does not derive `Serialize`/`Deserialize`.
    pub pending_recovery_secret: Option<SecretString>,
    /// The `nest_actor_id` (hex) of the box the admin selected on `nest_recovery`
    /// (`recover-box-item`), or `None` before a selection. The re-provision drive
    /// resolves this box's custodied seed in Rust (`DeploymentSeedEntry::seed_for`); the
    /// raw seed never crosses back to the client.
    pub recovery_selected_nest_id: Option<String>,
    /// The custodied box list rendered on `nest_recovery` (`recover-box-item`,
    /// indexed) — one `nest_actor_id` (hex) per box, resolved from this device's
    /// store and a reachable nest via the shared `deploymentSeeds()` getter and pushed in
    /// by the per-app glue (`set_recovery_boxes`). Held on the machine — like
    /// `vps.server_types` / `vps.locations` — so all seven apps render the same
    /// list from one snapshot (priority #2/#3). Empty (the default, or a
    /// not-yet-synced fresh device) → `recover-box-empty-message`. Carries only
    /// the public `nest_actor_id`, never the seed.
    pub recovery_boxes: Vec<String>,
}

impl State {
    /// **The one rule for which identity the wizard is acting as.**
    ///
    /// The wizard holds two slots — `generated_secret` (minted by
    /// `begin_create_identity`) and `imported_secret` (set by
    /// `confirm_imported_identity`, `submit_recovery_entry`, `seed_identity`) —
    /// and **no arm clears the other**, deliberately: a user who taps "create",
    /// looks around, goes back and then pastes the key they actually came with
    /// has both, and neither screen may destroy a secret the other may have
    /// shown them to write down. So precedence, not clearing, is what decides,
    /// and [`State::identity_origin`] is the decider: *the screen the user
    /// committed on wins.*
    ///
    /// **When each screen writes the origin is the whole content of that rule**,
    /// and the two screens reach it by opposite routes — deliberately, because
    /// they differ in when they fill their slot.
    /// [`crate::OnboardingMachine::begin_import_identity`] writes `Imported` on
    /// *entry* and fills `imported_secret` on *confirm*;
    /// [`crate::OnboardingMachine::begin_create_identity`] mints
    /// `generated_secret` on *entry* and writes `Created` on *confirm*
    /// ([`crate::OnboardingMachine::confirm_generated_identity`]). Import can
    /// afford the early write because the empty slot it leaves is itself the
    /// tell that nothing was committed — the fallback arm below reads it. Create
    /// has no such tell: its slot is full from the moment the screen opens, so
    /// an entry-time origin would be indistinguishable from a real commitment,
    /// and merely tapping "create" out of curiosity would displace an identity
    /// the user had already pasted and confirmed. Either way the invariant holds
    /// at the door: *entering* a screen never changes which identity is
    /// canonical, only confirming on it does.
    ///
    /// A consequence worth naming, because it is not visible from the arms
    /// below: `Some(Created)` now *implies* a non-empty `generated_secret` — the
    /// origin is written only once the confirm has one in hand, and nothing
    /// removes it but [`crate::OnboardingMachine::reset`], which clears the
    /// origin with it. So the `Created` arm's `or_else` is unreachable **by
    /// construction**, not merely uncovered by tests. It is kept for shape
    /// symmetry and because it would wake the moment anything cleared the slot;
    /// a change that introduces such a clear owes it a test.
    ///
    /// The fallback arm matters as much as the precedence. `begin_import_identity`
    /// sets the origin the moment the user *enters* the import screen, before
    /// anything is pasted, so `Imported` with an empty slot is an ordinary
    /// reachable state (entered the screen, went back) — reading it strictly
    /// would strand a user who has a perfectly good created identity with no
    /// secret at all. `identity_origin` is also deliberately left `None` by
    /// [`crate::OnboardingMachine::seed_identity`] (so Back routes to
    /// `identity_choice` — `onboarding.md` § 1 Identity), and that path seeds
    /// only the imported slot; the `None` arm therefore reads imported-first,
    /// which is exactly what it needs.
    ///
    /// Every caller reads this — the public
    /// [`crate::OnboardingMachine::effective_secret`] the apps' wizard terminal
    /// persists (`onboarding.md` § Long-term store contract, ratified
    /// 2026-08-27: *"the wizard has just authenticated with the identity, so
    /// `effective_secret()` always has it"*) and every wire call that
    /// authenticates: the handle check's silent challenge, the invite
    /// submit/recheck/redeem, the admin claim, the manual-DNS claim, the NAT-mode
    /// commit. Before this rule existed there were two accessors with **opposite**
    /// precedence and the authenticating calls used the inverted one, so the
    /// terminal persisted one identity while the nest had seen another. Pinned by
    /// `tests/identity_precedence.rs`.
    pub fn canonical_secret(&self) -> Option<SecretString> {
        match self.identity_origin {
            Some(IdentityOrigin::Created) => self
                .generated_secret
                .clone()
                .or_else(|| self.imported_secret.clone()),
            // `Imported`, and the `None` of a seeded identity, both name the
            // imported slot first.
            Some(IdentityOrigin::Imported) | None => self
                .imported_secret
                .clone()
                .or_else(|| self.generated_secret.clone()),
        }
    }
}

/// `key-material-hierarchy.md` § Carrier shape pin — reverting either field
/// to a bare `String` fails the build here (rather than only wherever a call
/// site happens to be strictly typed). Two fields, two pins: a single pin
/// only guards the one field it names.
const _STATE_GENERATED_SECRET_IS_REDACTED: fn(&State) -> &Option<SecretString> =
    |s| &s.generated_secret;
const _STATE_IMPORTED_SECRET_IS_REDACTED: fn(&State) -> &Option<SecretString> =
    |s| &s.imported_secret;

impl State {
    pub(crate) fn new() -> Self {
        Self {
            dns: DnsConfigState {
                same_provider_for_vps: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Set the onboarding error banner message **and** log it once.
    ///
    /// This is the *producer* side of an `error-message` display: the per-app
    /// onboarding views render `error_message()` reactively on every observer
    /// tick, so they can't log it (they'd re-log on every repaint —
    /// observability.md § What must be logged). Logging here, at the single
    /// transition that *sets* the error, fires once and — because the machine is
    /// shared Rust running under each app's installed `fauna-log` subscriber —
    /// covers the onboarding banner on all seven apps at once (priority #2). The
    /// message is a localized user-facing string (redaction-safe).
    pub(crate) fn set_error(&mut self, message: String) {
        tracing::warn!(target: "fauna_onboarding", "{message}");
        self.error_message = Some(message);
    }

    /// Reset every field a completed handle-check writes, so the HandleEntry
    /// page starts fresh after the identity changes. `domain_status()` is a
    /// derived view over the rich `HandleCheckSnapshot` (lives on the
    /// machine, a separate mutex, cleared alongside this — see
    /// `OnboardingMachine::reset_handle_check`), so clearing that snapshot's
    /// `outcome` to `HandleCheckOutcome::None` already resets it — nothing to
    /// clear here for it. `local_nest_reachable` is intentionally omitted:
    /// nothing writes it (it is always its `false` default), so it carries
    /// no stale handle-check state to clear.
    pub(crate) fn clear_handle_check(&mut self) {
        self.nest_url = String::new();
        self.node_mode_seed = None;
        self.handle_enclosing_zone = None;
    }

    /// Computes the domain part of the handle (`example.com` from
    /// `alice@example.com`). Returns `None` if the handle is malformed.
    pub(crate) fn handle_domain(&self) -> Option<&str> {
        let at = self.handle.find('@')?;
        if at == 0 || at == self.handle.len() - 1 {
            return None;
        }
        Some(&self.handle[at + 1..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_domain_extracts_after_at() {
        let mut s = State::new();
        s.handle = "alice@example.com".into();
        assert_eq!(s.handle_domain(), Some("example.com"));
    }

    #[test]
    fn handle_domain_none_for_malformed() {
        let mut s = State::new();
        s.handle = "no-at".into();
        assert_eq!(s.handle_domain(), None);

        s.handle = "@noprefix".into();
        assert_eq!(s.handle_domain(), None);

        s.handle = "noatend@".into();
        assert_eq!(s.handle_domain(), None);
    }

    #[test]
    fn dns_config_default_same_provider_true() {
        let s = State::new();
        assert!(s.dns.same_provider_for_vps);
        assert!(!s.dns.buy_domain);
        assert!(!s.dns.set_up_later);
    }

    #[test]
    fn step_defaults_to_identity_choice() {
        let s = State::new();
        assert_eq!(s.step, OnboardingStep::IdentityChoice);
    }

    /// `key-material-hierarchy.md` § Carrier shape — `{:?}` on `State` must
    /// never print the identity seed, in ANY of the three carriers the
    /// family now has. Mutation: reverting any field to a bare `String` reds
    /// this (the struct-level `#[derive(Debug)]` would then print it
    /// verbatim) as well as that field's build-time pin (two above this
    /// struct, one beside `RestoredPredecessorSeed` in `nest_api/types.rs`).
    /// Named for the two original carriers (a test asserting
    /// only two of three fields silently certified more than it checked),
    /// but the name still holds — it now covers what it names.
    #[test]
    fn state_debug_never_prints_the_identity_seed() {
        let seed_a = "ab".repeat(32);
        let seed_b = "cd".repeat(32);
        let seed_c = "ef".repeat(32);
        let mut s = State::new();
        s.generated_secret = Some(seed_a.clone().into());
        s.imported_secret = Some(seed_b.clone().into());
        s.restored_predecessors = vec![crate::nest_api::RestoredPredecessorSeed {
            actor_id_hex: "12".repeat(32),
            seed_hex: seed_c.clone().into(),
        }];
        let debug = format!("{s:?}");
        assert!(
            !debug.contains(&seed_a),
            "Debug output must redact generated_secret, got {debug:?}"
        );
        assert!(
            !debug.contains(&seed_b),
            "Debug output must redact imported_secret, got {debug:?}"
        );
        assert!(
            !debug.contains(&seed_c),
            "Debug output must redact restored_predecessors' seed_hex, got {debug:?}"
        );
    }
}
