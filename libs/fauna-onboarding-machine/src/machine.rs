use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use fauna_core::data::Timestamp;
use fauna_core::hex32::is_hex64;
use fauna_core::secret::SecretString;
use fauna_launch_machine::{AwaitingDnsRecord, PendingProvisionStore};
use fauna_provisioning::orchestrator::ServerOrigin;
use fauna_provisioning::progress::{CancelFlag as ProvisioningCancelFlag, ProvisioningSnapshot};
use fauna_provisioning::{Capability, PROVIDERS};

use crate::cancel::CancelFlag;
use crate::error::OnboardingError;
use crate::nest_api::RestoreSeedError;
use crate::observer::OnboardingObserver;
use crate::outcome::WizardOutcome;
use crate::snapshots::{
    AwaitingDnsState, AwaitingManualDnsSnapshot, ClaimCodeSnapshot, HandleCheckSnapshot,
    InviteRequestSnapshot, NatModeSnapshot, NatModeState,
};
use crate::state::{
    AgeClaimPlain, AgeNoncePlain, BoxRecoveryEntry, CapturedDnsCredential, CredentialForm,
    DnsConfigState, DnsRecordPlain, FieldMetaPlain, HostedAuthPrompt, HostedAuthState,
    IdentityOrigin, LocalizedText, NodeMode, OnboardingStep, State, VpsConfigState,
};

/// HTTP client for the handle-check **nest-health probe**, for *every* target —
/// local (bare IP / `localhost` / `.local`) and public registrable domain alike.
///
/// A nest routinely serves a self-signed TLS floor whose SANs don't cover the
/// address the client reached it on: a domainless floor is loopback-only (SANs
/// `localhost` + `127.0.0.1`), a domain-floor box reached by its LAN IP presents
/// a cert for the domain rather than the IP, and — the case that matters most —
/// **an internet nest is domainless right up until it is claimed**, because it
/// learns its name *from* the claim (`domains-and-tls-bootstrap.md` § Boot /
/// § Claim sets identity). A strict-WebPKI client (`reqwest::Client::new()`)
/// can't complete any of those handshakes, and the probe collapses a TLS failure
/// to `ConnectionRefused` → `RegisteredNoNest` — a **dead end**, since that
/// outcome only offers "I control this domain, set a nest up". That mis-report
/// hit every reachable bare-IP nest, and (until 2026-07-24) every *native* claim
/// of a fresh internet nest; the browser escaped it only because the user clicks
/// through the cert interstitial, which grants that origin an exception.
///
/// So this client routes through the SAME channel-binding TLS verifier the
/// anonymous WS-RPC pairing connection uses (`fauna_anon_client::tls_verify` in
/// `NoPinPolicy::AcceptProvisional` mode): it provisionally accepts the cert
/// (capturing its SPKI) and leaves real authentication to the downstream
/// `fauna.auth` channel-binding ceremony (`security.md` § Transport trust,
/// Axis 1). **The probe is only a reachability check, never the auth path** — it
/// carries no bearer and no secret, and the WS leg that follows it already
/// accepts provisionally-then-binds, so matching that posture here removes a
/// host-class special case rather than widening any trust. The strict `http`
/// client stays for the genuinely-public web services (DoH, the price proxy),
/// where WebPKI is the right check.
/// Which socket the reach override should dial for a captured box address, or
/// `None` when the address is unusable and the caller must fall back to the
/// override-less client.
///
/// Split out so the fallback is a named, tested decision rather than a silent
/// `else` inside the builder: a poll that quietly lost its override dials public
/// DNS instead of the box, which is indistinguishable from a working override
/// right up until it blackholes.
#[cfg(not(target_arch = "wasm32"))]
fn probe_reach_socket(ipv4: &str) -> Option<std::net::SocketAddr> {
    ipv4.parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| std::net::SocketAddr::new(ip, 443))
}

/// Per-dial ceilings for the liveness probe clients below.
///
/// The provisioning Online poll retries on an attempt *count*
/// (`ProvisionStep::default_retry_policy` — 480 attempts, 5s apart), which only
/// bounds anything if a single attempt is itself bounded. Until 2026-08-31
/// neither builder set a timeout, so a dial into a silent socket — an open port
/// with nothing serving yet, or a dropped SYN — hung indefinitely and one
/// attempt swallowed the whole step: measured live on windows against a real
/// Hetzner box, `attempt 2/480` after 1200s.
///
/// `/api/v1/health` is a tiny unauthenticated JSON response, so a box that
/// cannot complete TCP + TLS + a 200 inside `PROBE_REQUEST_TIMEOUT` is not
/// serving yet by any definition the poll cares about — and the next attempt is
/// only 5s away, so giving up early costs nothing and buys hundreds of real
/// attempts inside the same wall clock.
#[cfg(not(target_arch = "wasm32"))]
const PROBE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(not(target_arch = "wasm32"))]
const PROBE_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(not(target_arch = "wasm32"))]
fn nest_probe_client() -> reqwest::Client {
    use fauna_anon_client::tls_verify::{NoPinPolicy, PinResolver, dynamic_pinned_client_config};
    let no_pin: PinResolver = Arc::new(|| None);
    reqwest::Client::builder()
        .use_preconfigured_tls(dynamic_pinned_client_config(
            no_pin,
            NoPinPolicy::AcceptProvisional,
        ))
        .connect_timeout(PROBE_CONNECT_TIMEOUT)
        .timeout(PROBE_REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|e| {
            // `reqwest::Client::new()` carries NEITHER ceiling below (asserted at
            // `fauna-client-dns/src/lib.rs:3562`) and is strict WebPKI, which
            // cannot complete the self-signed floor-cert handshake this probe
            // exists to reach (`security.md` § Transport trust Axis 1 — the
            // provisional-accept posture is the declared design, not a gap to
            // harden away). Say exactly what was lost, then fall back to a
            // client that keeps the timeout ceilings — only the custom TLS is
            // unavailable — with `reqwest::Client::new()` (no ceilings) as the
            // last resort if even that plain builder fails, which is loud in
            // its own right below rather than silently dropping the ceilings
            // this message is about to claim are kept.
            tracing::error!(
                target: "fauna_onboarding",
                "nest probe client build failed ({e}): falling back to a client with \
                 NO provisional-TLS acceptance — a self-signed floor-cert nest is now \
                 unreachable by this probe; keeping the connect/request timeout ceilings",
            );
            reqwest::Client::builder()
                .connect_timeout(PROBE_CONNECT_TIMEOUT)
                .timeout(PROBE_REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|e| {
                    tracing::error!(
                        target: "fauna_onboarding",
                        "nest probe client rebuild also failed ({e}): falling back to a \
                         bare client with NO timeout ceilings either — this probe can now \
                         hang indefinitely on an unresponsive nest",
                    );
                    reqwest::Client::new()
                })
        })
}

/// WASM has no rustls backend — the browser owns TLS, so a self-signed nest is
/// unreachable from the web SPA without a user-added cert exception (the known
/// `web=hard` single-origin constraint). Fall back to the strict client: the
/// browser's own WebPKI check is the only one there is.
///
/// **The old rationale here — "in practice the user has already granted that
/// exception, since the SPA is served *by* the nest on the same origin" — is
/// false for the case this crate exists to serve.** A box the wizard just
/// provisioned is a *different* origin from the SPA the user is running, and it
/// is one nobody has ever visited, so there is no exception to inherit. What
/// makes the browser's dial work is not an exception but a genuinely trusted
/// cert: the box's **IP bridge cert**, a WebPKI Let's Encrypt certificate for
/// its own public IP, obtained from first boot
/// (`docs/goal/architecture/nest/tls-certificates.md` § B-IP) — which is why
/// the web arm dials `https://{ip}` literally rather than overriding anything
/// (`onboarding.md` § 6 *Reaching the box*). Until that cert exists a browser
/// reaches nothing, and no DNS wait changes it; the wizard's only door then is
/// the manual browser exception (§ "Almost ready" surface, *web fallback*).
///
/// The premise still holds for the case it was written for — an admin
/// onboarding against a nest that is *already* serving them the SPA — and that
/// case is why this arm is not an error.
#[cfg(target_arch = "wasm32")]
fn nest_probe_client() -> reqwest::Client {
    reqwest::Client::new()
}

/// Like [`nest_probe_client`] but with a **temporary DNS override** so the
/// client reaches `host` at the freshly-provisioned box's captured static IP
/// (`ipv4`), regardless of public DNS. Used for the provisioning Online poll: the
/// box is up on its IP the moment cloud-init finishes, but Hetzner's beta Cloud
/// DNS can take ~30 min to propagate the A record (`docs/goal/architecture/
/// testing.md` § Gap 3). SNI, `Host`, and cert-identity all stay `host` — only
/// the socket target is overridden — so the identity domain the admin claims is
/// unaffected. The override is MITM-safe: channel binding fuses the cert to the
/// nest identity (`security.md` § Transport trust Axis 1), so overriding the
/// *reach address* while the name stays `host` cannot be exploited. Falls back
/// to the override-less client if `ipv4` doesn't parse.
#[cfg(not(target_arch = "wasm32"))]
fn nest_probe_client_resolving(host: &str, ipv4: &str) -> reqwest::Client {
    use fauna_anon_client::tls_verify::{NoPinPolicy, PinResolver, dynamic_pinned_client_config};
    let Some(reach) = probe_reach_socket(ipv4) else {
        // Say so. Without this the poll below dials public DNS and nobody can
        // tell that from a working override until it blackholes.
        tracing::warn!(
            target: "fauna_onboarding",
            "nest probe for {host}: captured address {ipv4:?} is not dialable — \
             falling back to the override-less client (this poll now depends on \
             public DNS)",
        );
        return nest_probe_client();
    };
    tracing::info!(
        target: "fauna_onboarding",
        "nest probe for {host}: reaching the box at {reach} via the DNS override",
    );
    let no_pin: PinResolver = Arc::new(|| None);
    reqwest::Client::builder()
        .use_preconfigured_tls(dynamic_pinned_client_config(
            no_pin,
            NoPinPolicy::AcceptProvisional,
        ))
        .resolve(host, reach)
        .connect_timeout(PROBE_CONNECT_TIMEOUT)
        .timeout(PROBE_REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|e| {
            // Say so — the sibling unusable-address arm above already warns when
            // the override is unusable; a build failure silently drops it just as
            // completely, and until now did so silently.
            tracing::warn!(
                target: "fauna_onboarding",
                "nest probe client build failed for {host} ({e}): falling back to the \
                 override-less client — the DNS override to {reach} is lost, so this \
                 poll now depends on public DNS",
            );
            nest_probe_client()
        })
}

/// WASM: the browser owns DNS + TLS, so there is no client-side resolve override
/// (bare-IP onboarding stays browser-limited — see [`nest_probe_client`]).
#[cfg(target_arch = "wasm32")]
fn nest_probe_client_resolving(_host: &str, _ipv4: &str) -> reqwest::Client {
    nest_probe_client()
}

/// The `nest_actor_id` a box boots with when `FAUNA_DEPLOYMENT_SEED` carries
/// `seed_hex`: the Ed25519 public key derived from the 32-byte seed — the same
/// derivation the nest runs at boot (`deployment_key`), so the client knows the
/// identity a priori (`security.md` § Transport trust, the *Client-provisioned
/// box* row). `None` for a malformed seed.
///
/// **The malformed-seed tolerance below is scoped to the MINT path only.** On
/// mint, a malformed seed can only mean the local generator misbehaved, and
/// pinning a root derived from garbage would hard-fail the box's genuine first
/// contact — mirroring the nest's own tolerance (`decode_deployment_seed`
/// ignores garbage and mints its own identity). On the recovery-mode
/// re-provision path a malformed/corrupt seed means something different — the
/// custodied seed entry is corrupt — and callers there must NOT rely on
/// this `None` tolerance: they hold the selected box's own identity directly
/// and refuse the run on a derive mismatch instead (`run_provisioning_inner`'s
/// trust-root selection).
fn nest_actor_id_from_seed_hex(seed_hex: &str) -> Option<[u8; 32]> {
    let seed_bytes = hex::decode(seed_hex).ok()?;
    let seed = <[u8; 32]>::try_from(seed_bytes.as_slice()).ok()?;
    Some(
        ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes(),
    )
}

/// A persisted `nest_actor_id` (64 hex) back to the 32 bytes the identity root
/// holds; `None` for anything else, so a corrupt slot pins nothing rather than
/// pinning garbage.
fn decode_nest_actor_id_hex(id_hex: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(id_hex).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// What [`OnboardingMachine::request_age_nonce`] last minted: the nest it
/// asked, the nonce it got, and the attestation platforms that reply listed.
#[derive(Debug, Clone)]
struct AgeNonceMint {
    nest_url: String,
    nonce_hex: String,
    attestation_platforms: Vec<String>,
}

/// The single shared onboarding wizard state machine. Each app holds
/// `Arc<OnboardingMachine>`, observes via the registered `OnboardingObserver`,
/// and mutates via the methods exposed below.
///
/// Wizard state is in-memory only — there is no persistence layer here.
/// Identity-confirmation methods return the secret hex so the per-app
/// glue can write it to its long-term store immediately, and `seed_identity`
/// lets app-launch code pre-populate the wizard at HandleEntry from a
/// previously-stored secret.
///
/// Uses `std::sync::Mutex` (not `tokio::sync::Mutex`) so getters and sync
/// mutations work from any thread context — including UI threads on native
/// apps and `#[tokio::test]` runtimes. Async methods take a snapshot under
/// the lock, drop the lock, do IO, then re-acquire to apply changes; the lock
/// is never held across an `await`.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct OnboardingMachine {
    pub(crate) state: Mutex<State>,
    pub(crate) observer: Arc<dyn OnboardingObserver>,
    pub(crate) http: reqwest::Client,
    /// Self-signed-tolerant client for the handle-check nest-health probe — see
    /// [`nest_probe_client`]. Used for **every** probe target, local and public
    /// alike: the probe is a reachability check, and a nest on its self-signed
    /// floor is the normal pre-claim state of an internet nest, not an anomaly.
    pub(crate) http_nest_probe: reqwest::Client,
    /// Trait abstraction over the wizard's nest-side surface. All
    /// `fauna.{setup.status,auth.claim_admin,account.invite_request.*,
    /// account.invite_code.verify,account.register,setup.nat_mode}` calls go
    /// through this. Production code uses `WsNestApi` (pre-identity WS-RPC);
    /// tests construct `OnboardingMachine` via
    /// `with_nest_api(observer, Arc<FakeNestApi>)`.
    pub(crate) nest_api: Arc<dyn crate::nest_api::NestApi>,
    handle_check: Mutex<HandleCheckSnapshot>,
    invite_request: Mutex<InviteRequestSnapshot>,
    claim_code: Mutex<ClaimCodeSnapshot>,
    /// Snapshot for the `nat_mode_choice` page — the single, terminal
    /// admin-path setup step, reached on claim completion.
    /// Per `docs/goal/behavior/onboarding.md` § 3b-bis.
    nat_mode: Mutex<NatModeSnapshot>,
    /// Snapshot for the post-provisioning "Almost ready" surface. Entered
    /// after the deferred-DNS path exits with `WizardOutcome::AwaitingManualDns`;
    /// `recheck_manual_dns` polls the freshly-provisioned nest and drives
    /// this snapshot until the claim completes. Per
    /// `docs/goal/behavior/onboarding.md` § "Wizard exit handling".
    awaiting_manual_dns: Mutex<AwaitingManualDnsSnapshot>,
    outcome: Mutex<Option<WizardOutcome>>,
    cancel: CancelFlag,
    /// Snapshot driving the nest_provisioning page. Mutated by the
    /// orchestrator's `run_step` loop; clients read via
    /// `provisioning_snapshot()`.
    pub(crate) provisioning: Mutex<ProvisioningSnapshot>,
    /// Soft-cancel flag observed by the provisioning task at every step
    /// boundary and retry iteration.
    pub(crate) provisioning_cancel: ProvisioningCancelFlag,
    /// Test-only override map: provider id (e.g. "vps", "dns", "nest")
    /// → base URL. None in production. Used by E2E tests to redirect
    /// provider HTTP calls to a local fake-cloud server.
    ///
    /// Interior-mutable (`RwLock`) so the native E2E bridge can apply the
    /// override at runtime via `set_provider_base_urls` — web reconstructs
    /// the wizard machine on a reload, but native apps (linux/apple/…)
    /// hold one long-lived machine instance the page is already bound to, so
    /// they set the override in place instead. Affects only the HTTP
    /// provisioning surface (vps/dns/nest-health, all read via
    /// `provider_base_url` at call time); the WS-RPC `nest_api` construction is
    /// construction-time (the base-URL redirect), but the resolve override
    /// below is mutated afterward.
    provider_base_urls: RwLock<Option<HashMap<String, String>>>,
    /// Whether [`Self::provider_base_url`]'s `"nest"` read-back may fall back to
    /// the **process-global** launch-side dial override
    /// (`fauna_launch_machine::dial`) when this machine holds no `"nest"` entry
    /// of its own. `true` for every machine a production constructor builds —
    /// [`Self::new`], `new_with_persistence`, `new_with_provider_base_urls`, all
    /// routed through [`Self::build`] — because that fallback is the whole point
    /// of the mirror: an app that rebuilds its wizard mints a FRESH machine
    /// while the harness installs the map once per fixture, and without the
    /// read-back the pre-identity probe goes dark at exactly that moment
    /// (`tests/nest_dial_override_mirror.rs`).
    ///
    /// `false` for the two test-only constructors that are handed their own
    /// [`crate::nest_api::NestApi`] (`with_nest_api`,
    /// `with_nest_api_and_provider_base_urls` — zero production call sites, and
    /// no app anywhere injects a fake transport). Such a machine shares no nest
    /// with whatever installed the global, so inheriting it is never right — and
    /// because the global's blast radius is the whole process, one sibling test
    /// installing an override otherwise reaches into every other test's freshly
    /// minted machine in the same binary. Measured cost of leaving it open
    /// (2026-08-30): `tests/recovery_entry_ceremony.rs` failed 8 of its 21 tests
    /// on `origin/main`, every failure asserting the same leaked URL — but as a
    /// RACE, 2 runs in 40 of the same unchanged binary, with `--test-threads=1`
    /// green every time. So one run proved nothing, and the crate's `tests/`
    /// directory was executed by no gate at all, so nothing reported any of it.
    ///
    /// Deliberately NOT the fix of deleting the global: the mirror is a pinned
    /// contract paid for by a real surviving bug (`fauna_launch_machine::dial`'s
    /// module docs). This narrows who inherits it, and leaves who installs it
    /// alone.
    #[cfg_attr(
        not(any(
            test,
            debug_assertions,
            feature = "test-helpers",
            feature = "e2e-agent"
        )),
        allow(dead_code)
    )]
    inherits_dial_override: bool,
    /// The port a *loopback* nest is reached on for a bare (no-port) handle like
    /// `test@localhost`. Default 443 (the network-reachable nest;
    /// `installers/windows.md`); the same-box app (Option A) or an e2e test injects
    /// a different one via [`Self::set_local_nest_port`]. NOT a human config surface
    /// — bucket-1 constant/IPC: a Rust default or
    /// an artifact/test-set value, never a knob a person edits.
    local_nest_port: RwLock<u16>,
    /// A test-only PIN of the exact nest Docker image tag cloud-init writes for
    /// a provisioned box, overriding the tag the user's update channel maps to.
    /// `None` everywhere but a test: the tag a real run uses is
    /// `provision_update_channel().image_tag()` — the
    /// `vps-config-update-channel-row` choice
    /// (`docs/goal/behavior/onboarding-provisioning.md` § 5). The pin exists for
    /// what no channel can name: the live-provision e2e sets it via
    /// [`Self::set_provision_image_tag`] (sourced from `FAUNA_E2E_IMAGE_TAG`) to
    /// validate one specific image, e.g. an old `sha-` tag.
    provision_image_tag: RwLock<Option<String>>,
    /// A claim code pinned for the next provisioning run instead of the minted
    /// one — `None` everywhere but a test. **Production must never set this:**
    /// the code is a bearer secret whose whole strength is that it is freshly
    /// random (`fauna_core::claim_code`), and the nest-side throttles are sized
    /// against that. It exists because a tier_3 journey points the run at a REAL
    /// nest the harness already booted with a claim code of its own, and the
    /// run's own minted code could never match it
    /// ([`Self::set_provision_claim_code_for_test`]).
    provision_claim_code: RwLock<Option<String>>,
    /// Provider-side labels/tags attached to a provisioned VPS at create time,
    /// **in addition to** the constant `managed-by=fauna` marker the
    /// orchestrator unions in at its create chokepoint
    /// (`fauna_provisioning::vps::MANAGED_BY_LABEL` — every fauna-provisioned
    /// box carries it; `vps.md` § Uninstall). Empty in production. The
    /// live-provision e2e sets `[("fauna-e2e","1")]` via
    /// [`Self::set_provision_labels`] so its throwaway boxes are sweepable by
    /// `label_selector` instead of a name-prefix + age heuristic. NOT a human
    /// config surface — bucket-1 IPC (same bucket as `local_nest_port`).
    provision_labels: RwLock<Vec<(String, String)>>,
    /// `hosted-auth` sign-ins in flight between [`Self::hosted_auth_begin`]
    /// and [`Self::hosted_auth_wait`], keyed by (form, field id). The RFC 8628
    /// `device_code` is a bearer-like secret the app never needs, so it lives
    /// here and never in the snapshot.
    hosted_auth_pending: Mutex<HashMap<(CredentialForm, String), PendingDeviceAuth>>,
    /// The app's age claim for the admission ahead (`family-safety.md` § The
    /// account age band) — rides the invite-request submit and the register
    /// bodies verbatim. Set by the mobile apps from their store age signal
    /// ([`Self::set_age_claim`]); `None` everywhere else, and after `reset`.
    age_claim: RwLock<Option<fauna_protocol::age::AgeClaim>>,
    /// The last `request_age_nonce` mint — which nest, which nonce, which
    /// platforms it said it can verify. The guard [`Self::age_claim_to_send`]
    /// reads; cleared with `age_claim` on `reset`.
    age_nonce_mint: RwLock<Option<AgeNonceMint>>,
    /// (host, ip) captured once provisioning succeeds (`stash_provisioning_result`)
    /// — shared with the production `WsNestApi` (constructed alongside it in
    /// [`Self::new`]) so its per-call connect can reach that host by IP before
    /// its DNS record has propagated. `None` until a provisioning run succeeds;
    /// cleared on [`Self::reset`].
    nest_resolve_override: Arc<RwLock<Option<(String, std::net::IpAddr)>>>,
    /// (host, expected `nest_actor_id`) — the **public** identity derived from
    /// the deployment seed this client injects at provision, captured in
    /// `run_provisioning_inner` and shared with the production `WsNestApi` as
    /// the Axis-2 pre-resolved identity root for that host's pre-identity
    /// connections (first-contact trust: the box must present the identity we
    /// provisioned it with — security.md § Transport trust; design tracked
    /// internally). Holds
    /// only the derived public key, never the seed (
    /// the claim reply stays custody ground truth). Kept for the machine's
    /// whole bootstrap (claim → storage-mode → mail-enable all graduate against
    /// it; the box's deployment identity doesn't rotate at claim), cleared on
    /// [`Self::reset`].
    nest_expected_identity: crate::nest_api::ExpectedNestIdentity,
    /// Resolves a lost box's custodied deployment seed + domain — this device's
    /// own account store joined with a cold read from a reachable nest — for the
    /// step-4 cloud re-provision drive (`resolve_deployment_seed_and_domain` /
    /// `run_provisioning_inner`). The per-target production reader is self-wired in
    /// [`Self::new`] over the platform store root (`recovery_config::production_reader`),
    /// re-rooted by [`Self::set_store_container_dir`] on a sandboxed phone. Interior-
    /// mutable so a unit test can swap in a fake without a live nest. Not a UniFFI
    /// surface — a Rust `dyn` seam the foreign side never touches (see
    /// `recovery_config` for why it is self-wired, not a constructor param).
    recovery_config_reader: RwLock<Arc<dyn crate::recovery_config::RecoveryConfigReader>>,
    /// The pending-provision slot's writer (`docs/goal/behavior/onboarding.md`
    /// § 6 *The pending-provision slot*) — the machine's **only** durable side
    /// effect outside its own state.
    ///
    /// Injected rather than called directly because this crate deliberately has
    /// no `fauna-client-accounts` dependency: the slot lives in the per-actor
    /// account registry, which is a layer the wizard state machine does not (and
    /// should not) know about. `None` in tests that do not exercise the slot; a
    /// production app that leaves it `None` loses the crash resume, which is why
    /// `provision_slot_is_wired_on_every_production_app` pins the six
    /// construction sites.
    pending_provision: RwLock<Option<Arc<dyn PendingProvisionStore>>>,
    /// The box's reach address, published by the orchestrator's
    /// `on_server_ready` the moment `create_server` returns — the only point at
    /// which it is knowable, since nothing reaches the shared snapshot until the
    /// whole run succeeds (§ 6 *Reaching the box*).
    provision_reach_ipv4: RwLock<Option<String>>,
    /// The box this client is mid-way through provisioning: the row it last
    /// wrote to the pending-provision slot (`docs/goal/behavior/onboarding.md`
    /// § 6 *The pending-provision slot*), kept beside the store's copy so the
    /// machine itself remembers what it built — the claim code the box boots
    /// with, the identity it was injected with, its reach address.
    ///
    /// It is what makes **Retry resume the same box instead of poisoning it**:
    /// the orchestrator's Server step is a name-only pre-flight, so a re-run
    /// of the same domain finds the box an earlier run built and skips
    /// `create_server` — but that box boots with the EARLIER run's cloud-init,
    /// and a run that minted a fresh code and seed could then neither verify
    /// its identity (`nest_actor_id is not the expected nest`, hard-fail by
    /// design) nor claim it. `run_provisioning_inner` reuses this row for the
    /// same `(nest_url, handle)` and re-holds its identity; an app that wired
    /// no store still gets the in-session half from this field alone.
    ///
    /// Lifetime mirrors the slot's, not the run's: written before
    /// `create_server`, completed by `note_server_ready`, **survives
    /// `reset()`** (a "start over" onto the same domain must find the same
    /// box — the slot survives for exactly that reason), seeded by a relaunch's
    /// `seed_awaiting_manual_dns_record`, and dropped only when the box is
    /// claimed (`forget_pending_provision_row`).
    pending_provision_row: RwLock<Option<AwaitingDnsRecord>>,
    /// `(domain, nest_actor_id)` of a box this machine has **already claimed** —
    /// the one fact [`Self::forget_pending_provision_row`] must not throw away
    /// with the row.
    ///
    /// Forgetting the row is right: custody is discharged at the claim and the
    /// claim code must not outlive it. But the row was also the machine's only
    /// memory of *which box lives at this domain*, and the orchestrator's Server
    /// step is a name-only pre-flight — so the very next run finds that same box
    /// and skips `create_server`. Without this field that run held a fresh
    /// seed's identity as the first-contact root and the next pre-identity dial
    /// refused the box it had itself just claimed
    /// (`nest_actor_id is not the expected nest`) — measured live on windows
    /// 2026-09-04, and a client-reachable wedge `nest/common.md`
    /// § Client-state recoverability forbids, since Retry is a live button on
    /// the provisioning page.
    ///
    /// Carries no claim code and confers no claim: it is read only by
    /// [`Self::note_server_ready`]'s **found**-box arm, and a run that genuinely
    /// CREATES a new box overwrites the held root with that box's fresh identity
    /// on the same line — so a genuine "provision a new box here" is unaffected.
    ///
    /// **Survives [`Self::reset`]**, for the reason `pending_provision_row`
    /// does and `nest_expected_identity` does not: a "start over" onto the same
    /// domain finds the same box, so the machine's memory of which box that is
    /// must outlive the wizard's own state. The held root is per-run; this is
    /// per-box.
    claimed_box_identity: RwLock<Option<(String, [u8; 32])>>,
}

/// Rust-only injection of the loopback nest port — deliberately NOT a UniFFI
/// export, because it is not a human config surface (the only config surface
/// is the apps; there is no operator). The
/// e2e onboarding test sets it to a random free port; the C# desktop app's Option-A
/// wiring will add the UniFFI setter when it lands.
impl OnboardingMachine {
    /// Point a bare (no-port) loopback handle (`test@localhost`) at a local nest on
    /// `port` instead of the default 443. The e2e onboarding test boots a nest on a
    /// random free port and calls this so the full flow runs in the harness without
    /// privileged `:443`. **No production caller exists today** — the same-box app's
    /// Option-A wiring (see the `impl` doc above) is still unwritten; that commit is
    /// what un-gates this fn, the same way its three siblings below were un-gated
    /// only once their own e2e-only status was confirmed.
    /// **Compiled out of release artifacts** (convention 15 rule (a),
    /// `e2e-automation-surface-gating.md` § The convention): this is an
    /// e2e-only seam, so the crate's visibility gate — not merely the
    /// dispatcher's — is what keeps it out of a shipped binary.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_local_nest_port(&self, port: u16) {
        *self
            .local_nest_port
            .write()
            .unwrap_or_else(|e| e.into_inner()) = port;
    }

    /// Pin the exact nest Docker image tag cloud-init writes for the next
    /// provisioning run, overriding the update channel's tag. Rust-only IPC
    /// injection, deliberately NOT a UniFFI export — the human choice is the
    /// update channel (`set_provision_update_channel`); this names a single
    /// image no channel can. The live-provision e2e sets it from
    /// `FAUNA_E2E_IMAGE_TAG` to validate a specific image.
    /// **Compiled out of release artifacts** (convention 15 rule (a),
    /// `e2e-automation-surface-gating.md` § The convention): this is an
    /// e2e-only seam, so the crate's visibility gate — not merely the
    /// dispatcher's — is what keeps it out of a shipped binary.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_provision_image_tag(&self, tag: String) {
        *self
            .provision_image_tag
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(tag);
    }

    /// Pin the claim code the next provisioning run persists and presents,
    /// instead of minting a fresh one. Rust-only, deliberately NOT a UniFFI
    /// export, and named for what it is: **only a test may call this.**
    ///
    /// The tier_3 journey it exists for points the run's `nest` base URL at a
    /// real local nest, which booted with the harness's own claim code already
    /// on disk (`tests/common/nest.py`). The run's minted code could never match
    /// that, and pointing the fix the other way — having the harness write the
    /// machine's code into the nest — would mean racing a file write against the
    /// claim, since the code does not exist until mid-run. So the harness pins
    /// the code it already knows.
    ///
    /// What it does NOT change is everything else about the slot: the pinned
    /// code still goes through `mint_and_persist_pending_provision`'s
    /// custody-precedes-dispatch write and read-back, so the journey exercises
    /// that path rather than stepping around it.
    /// **Compiled out of release artifacts** (convention 15 rule (a),
    /// `e2e-automation-surface-gating.md` § The convention): this is an
    /// e2e-only seam, so the crate's visibility gate — not merely the
    /// dispatcher's — is what keeps it out of a shipped binary.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_provision_claim_code_for_test(&self, code: String) {
        *self
            .provision_claim_code
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(code);
    }

    /// The claim code a run should use: the test pin if one is set, else `None`
    /// meaning "mint a fresh one" — the production answer.
    fn pinned_claim_code(&self) -> Option<String> {
        self.provision_claim_code
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Set the provider-side labels/tags attached to the next provisioned VPS
    /// (empty by default), on top of the constant `managed-by=fauna` marker the
    /// orchestrator adds to every box. Rust-only IPC injection, deliberately NOT
    /// a UniFFI export — not a human config surface. The live-provision e2e sets
    /// `[("fauna-e2e","1")]` so its throwaway boxes are sweepable by
    /// `label_selector`; production passes no extra labels.
    /// **Compiled out of release artifacts** (convention 15 rule (a),
    /// `e2e-automation-surface-gating.md` § The convention): this is an
    /// e2e-only seam, so the crate's visibility gate — not merely the
    /// dispatcher's — is what keeps it out of a shipped binary.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_provision_labels(&self, labels: Vec<(String, String)>) {
        *self
            .provision_labels
            .write()
            .unwrap_or_else(|e| e.into_inner()) = labels;
    }

    /// The real constructor body, shared by the exported [`Self::new`] (which
    /// passes `None`) and the test-only [`Self::new_with_provider_base_urls`].
    /// Private, so no flavor of any binding can reach the override parameter.
    fn build(
        observer: Arc<dyn OnboardingObserver>,
        provider_base_urls: Option<HashMap<String, String>>,
    ) -> Arc<Self> {
        // The wizard's nest surface rides the pre-identity (anonymous) WS-RPC
        // connection — the onboarding machine is its first client consumer
        // (transport.md § Pre-identity). `WsNestApi` opens a fresh connection
        // per call and honors the same `provider_base_urls["nest"]` override the
        // old HTTP impl did (which was removed once every app rode WS-RPC).
        let nest_resolve_override = Arc::new(RwLock::new(None));
        let nest_expected_identity = Arc::new(RwLock::new(None));
        let nest_api: Arc<dyn crate::nest_api::NestApi> =
            Arc::new(crate::nest_api::WsNestApi::new(
                provider_base_urls.clone(),
                nest_resolve_override.clone(),
                nest_expected_identity.clone(),
            ));
        Self::assemble(
            observer,
            nest_api,
            RwLock::new(provider_base_urls),
            nest_resolve_override,
            nest_expected_identity,
            // The production path: this machine drives the real `WsNestApi`, so
            // it is exactly the caller the dial mirror's read-back half exists
            // for (see the field's own note).
            true,
        )
    }

    /// The field-literal skeleton [`Self::build`] and [`Self::with_nest_api`]
    /// both independently hand-rolled verbatim (found 2026-08-25, the
    /// shared-Rust demand-driven scout) — every field except the six that
    /// genuinely vary by caller (`observer`, `nest_api`, the three
    /// nest-dial-override fields `build` threads through `WsNestApi::new`
    /// but `with_nest_api` has no equivalent construction step for, and
    /// `inherits_dial_override`, which is precisely the production-vs-injected-
    /// transport distinction the two callers embody).
    fn assemble(
        observer: Arc<dyn OnboardingObserver>,
        nest_api: Arc<dyn crate::nest_api::NestApi>,
        provider_base_urls: RwLock<Option<HashMap<String, String>>>,
        nest_resolve_override: Arc<RwLock<Option<(String, std::net::IpAddr)>>>,
        nest_expected_identity: crate::nest_api::ExpectedNestIdentity,
        inherits_dial_override: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new()),
            observer,
            http: reqwest::Client::new(),
            http_nest_probe: nest_probe_client(),
            nest_api,
            nest_resolve_override,
            handle_check: Mutex::new(HandleCheckSnapshot::idle()),
            invite_request: Mutex::new(InviteRequestSnapshot::idle()),
            claim_code: Mutex::new(ClaimCodeSnapshot::idle()),
            nat_mode: Mutex::new(NatModeSnapshot::idle()),
            awaiting_manual_dns: Mutex::new(AwaitingManualDnsSnapshot::idle()),
            outcome: Mutex::new(None),
            cancel: CancelFlag::new(),
            provisioning: Mutex::new(ProvisioningSnapshot::idle()),
            provisioning_cancel: ProvisioningCancelFlag::new(),
            provider_base_urls,
            local_nest_port: RwLock::new(fauna_provisioning::probe::LOCAL_NEST_DEFAULT_PORT),
            provision_image_tag: RwLock::new(None),
            provision_claim_code: RwLock::new(None),
            provision_labels: RwLock::new(Vec::new()),
            hosted_auth_pending: Mutex::new(HashMap::new()),
            age_claim: RwLock::new(None),
            age_nonce_mint: RwLock::new(None),
            nest_expected_identity,
            inherits_dial_override,
            recovery_config_reader: RwLock::new(crate::recovery_config::production_reader(
                fauna_account_plane::deployment_seed_recovery::StoreRoot::platform(),
            )),
            pending_provision: RwLock::new(None),
            provision_reach_ipv4: RwLock::new(None),
            pending_provision_row: RwLock::new(None),
            claimed_box_identity: RwLock::new(None),
        })
    }

    /// Reads the provider base-URL override for `key` (e.g. `"vps"`, `"dns"`,
    /// `"nest"`), or `None` — which is always the answer in a production
    /// artifact, because nothing in one can install the map (see
    /// [`Self::new_with_provider_base_urls`]).
    ///
    /// Always compiled: production request paths call it unconditionally
    /// (`effective_nest_url`, the vps/dns orchestrator hand-off, the
    /// provisioning stash) and take the `None` branch. Deliberately NOT a
    /// UniFFI export — the e2e read-back goes through the `test-helpers`-gated
    /// bridge arm (`call_machine_method_with_result("provider_base_url", …)`),
    /// so exporting it too would put an automation reader in every release
    /// artifact for no consumer (`testing.md` convention 15).
    pub fn provider_base_url(&self, key: String) -> Option<String> {
        let local = self
            .provider_base_urls
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|m| m.get(&key).cloned());
        if local.is_some() {
            return local;
        }
        // The `"nest"` read-back half of the dial mirror
        // ([`Self::set_provider_base_urls`] installs it; `fauna_launch_machine`
        // holds it). The map is per-machine, but a native app that rebuilds its
        // wizard mints a FRESH machine — linux does exactly that on every
        // `driver.reset()` — while the harness installs the map only once per
        // fixture. Without this fallback the pre-identity probe went dark at that
        // moment and probed the unresolvable typed domain, even though the
        // mirrored launch-side override was still pointing at the fixture nest:
        // the split brain the setter's own note forbids, running backwards.
        // Pinned by `tests/nest_dial_override_mirror.rs`; the measured cost of
        // leaving it open was this surviving case.
        //
        // `"nest"` only, and gated exactly as the setter is: no other key has a
        // process-global home, and a production artifact carries neither half.
        //
        // `inherits_dial_override` keeps this to the machines the mirror is FOR:
        // a production constructor's. A machine handed its own `NestApi` shares
        // no nest with whatever installed the global, and the global's blast
        // radius is the whole process — see that field's note for the racy
        // 8-of-21 failure this guard removes.
        #[cfg(any(
            test,
            debug_assertions,
            feature = "test-helpers",
            feature = "e2e-agent"
        ))]
        if key == "nest" && self.inherits_dial_override {
            return fauna_launch_machine::nest_dial_override();
        }
        local
    }

    /// Clear every field of the total-box-loss recovery branch. Called when the
    /// wizard leaves the branch (a normal create/import entry, or backing out to
    /// IdentityChoice) so a stale `recovery_intent` can't mis-route a later
    /// non-recovery import to `NestRecovery`.
    fn clear_recovery(s: &mut State) {
        s.recovery_intent = false;
        s.recovery_came_from = None;
        s.recovery_selected_nest_id = None;
        s.recovery_boxes = Vec::new();
    }
}

// The recovery-entry screen's predecessor read-outs, exported over UniFFI for
// the FFI apps' signed-in handoff (apple first; `RestoredPredecessorSeed`
// carries its `uniffi::Record` derive for exactly this consumer — a uniffi
// type name is global across the flat Swift/C# module, so it was minted with
// its consumer, not ahead).
#[cfg_attr(feature = "uniffi", uniffi::export)]
impl OnboardingMachine {
    /// Predecessor identity seeds a phrase-only restore recovered alongside the
    /// account's own (`identity-succession.md` § Seed escrow). **Empty in every
    /// ordinary onboarding**; non-empty only when the restored account is mid
    /// corpus re-seal after an identity succession.
    ///
    /// The client persists these into its account registry beside the restored
    /// identity — they are what lets the re-seal driver open a corpus still
    /// sealed under a predecessor. Read at the same point as
    /// [`Self::effective_secret`]: this machine holds no store, so an
    /// unread value is simply lost when the wizard ends.
    pub fn restored_predecessors(&self) -> Vec<crate::nest_api::RestoredPredecessorSeed> {
        self.with_state(|s| s.restored_predecessors.clone())
    }

    /// Why the restore recovered no predecessor material even though the blob
    /// carried a section — see
    /// [`RecoveryEntryOutcome::RestoredPredecessorsLost`].
    pub fn restored_predecessors_unreadable(&self) -> Option<String> {
        self.with_state(|s| s.restored_predecessors_unreadable.clone())
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl OnboardingMachine {
    /// Creates a new machine. Always starts fresh at `IdentityChoice`;
    /// callers that want to resume a prior identity should construct, then
    /// call `seed_identity(secret)` to jump to `HandleEntry`.
    ///
    /// This is the **only** constructor a production artifact carries, and it
    /// takes no override map — every native app already passed `None` there, so
    /// the parameter was pure automation surface riding the exported UniFFI
    /// signature (`testing.md` convention 15). E2E builds reach the map through
    /// the `test-helpers`-gated pair further down this file: a construction-time
    /// twin of this constructor (what web needs, since it rebuilds the machine
    /// on reload) and a runtime setter driven by the bridge (what the
    /// long-lived native machines need).
    ///
    /// Deliberately phrased without naming either gated symbol: UniFFI copies
    /// doc comments into the generated metadata, so a name written here lands
    /// in every release artifact's `strings` and muddies the very grep that
    /// convention 15 uses as its absence proof.
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new(observer: Arc<dyn OnboardingObserver>) -> Arc<Self> {
        Self::build(observer, None)
    }

    /// [`Self::new`] plus the pending-provision slot's writer — **the form every
    /// production app builds the wizard with** (`docs/goal/behavior/onboarding.md`
    /// § 6 *The pending-provision slot*).
    ///
    /// A second constructor rather than a widened `new`, following
    /// [`Self::new_with_provider_base_urls`]: `new` has ~30 in-repo test call
    /// sites that want the plain form and no slot, and widening it would make
    /// every one of them carry a `None` that says nothing.
    ///
    /// The cost of a *second* constructor is that an app which forgets to switch
    /// loses the crash resume silently — which is exactly the failure class this
    /// row exists to remove — so the six production sites are pinned by
    /// `provision_slot_is_wired_on_every_production_app`, and that test fails on
    /// the machine of whoever drops one rather than in a user's crashed wizard.
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new_with_persistence(
        observer: Arc<dyn OnboardingObserver>,
        persistence: Arc<dyn PendingProvisionStore>,
    ) -> Arc<Self> {
        let machine = Self::build(observer, None);
        *machine
            .pending_provision
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(persistence);
        machine
    }

    /// The box's reach address once `create_server` has returned, else `None`
    /// (§ 6 *Reaching the box*). Survives a `reset()` no more and no less than
    /// the rest of the run state does — see [`Self::reset`].
    pub fn provision_reach_ipv4(&self) -> Option<String> {
        self.provision_reach_ipv4
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Discards all state and resets to `IdentityChoice`.
    ///
    /// Also clears the provisioning snapshot + cancel flag so a "start over"
    /// (or the E2E `reset` between tests) returns to a clean Idle provisioning
    /// page. Native apps reuse one long-lived machine across resets, so
    /// without this a prior run's terminal snapshot (e.g. `Succeeded`) would
    /// persist and hide the `provisioning-start-button` (visible only while
    /// `overall == Idle`) — web avoids it only because its reset reconstructs
    /// the machine. The claim-code snapshot is cleared for the identical reason:
    /// its terminal `Claimed` state carries `submit_enabled == false`, so a
    /// process that already claimed a nest would render the next claim's Submit
    /// button permanently dead. (Its route in re-seeds too — see the
    /// `UnregisteredUnclaimedNest` arm — but "start over" must not depend on
    /// which door the user next walks through.) The remaining transient
    /// snapshots (handle-check, invite) are re-seeded by their own setters at
    /// the start of every use, so they don't need clearing here.
    pub fn reset(&self) {
        {
            let mut guard = self.state.lock().unwrap();
            // Per-app *capability* declarations are not run state and must
            // survive: the app declares them once when it builds the machine
            // (tui's `Wizard::new` → `set_renders_recovery_kit(true)`) and
            // nothing ever re-declares them, so a reset that dropped one would
            // drop it for the rest of that process's life — silently routing
            // onboarding around a screen the app does render. A factory reset
            // clears the user's PROGRESS, never what the app is capable of.
            let renders_recovery_kit = guard.renders_recovery_kit;
            let renders_trust_prompt = guard.renders_trust_prompt;
            *guard = State::new();
            guard.renders_recovery_kit = renders_recovery_kit;
            guard.renders_trust_prompt = renders_trust_prompt;
        }
        // The wizard OUTCOME is progress, and a factory reset clears progress.
        // It lives outside `State`, so the wholesale `State::new()` above cannot
        // reach it — the same shape as the `renders_recovery_kit` bug, but the
        // opposite polarity: that field had to SURVIVE reset (a capability the
        // app declares once), this one must NOT (a result the user produced).
        //
        // Leaving it set strands every app that renders off `wizard_outcome()`
        // rather than off `step`: the "Almost ready" (AwaitingManualDns) surface
        // is deliberately not an `OnboardingStep` (`onboarding.md` § "Wizard exit
        // handling"), so a stale outcome repaints it over a machine that believes
        // it is back at `IdentityChoice` — and nothing re-clears it for the rest
        // of the process's life, so the user never reaches identity-choice again.
        // Pinned by `tests/awaiting_manual_dns.rs::reset_clears_the_wizard_outcome`.
        *self.outcome.lock().unwrap() = None;
        *self.provisioning.lock().unwrap() = ProvisioningSnapshot::idle();
        *self.claim_code.lock().unwrap() = crate::snapshots::ClaimCodeSnapshot::idle();
        self.provisioning_cancel.reset();
        // The age claim is progress too — it names the identity it was
        // attested for, which a fresh run replaces.
        *self.age_claim.write().unwrap_or_else(|e| e.into_inner()) = None;
        *self
            .age_nonce_mint
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        // Drop any captured reach-by-IP override: a "start over" may pick a
        // different domain, and a stale (host, ip) pair surviving into a fresh
        // run risks dialing a torn-down box's old address instead of a new one
        // provisioned for the same domain.
        *self
            .nest_resolve_override
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        // The reach address the run captured, for the same reason and one step
        // further: this one is read at the `LoggedIn` terminal and PERSISTED as
        // the account's reach hint (`onboarding.md` § Reach hint), so a stale
        // value surviving a "start over" would not merely misdial in-session —
        // it would durably record the wrong address against an account that,
        // after the reset, may be signing in to a nest it never provisioned.
        //
        // Safe to drop even when the run below resumes the same box: the
        // orchestrator's `on_server_ready` fires again on the resumed run (the
        // idempotent pre-flight returns the existing instance), so the address
        // comes back the moment it is knowable. Unlike `pending_provision_row`,
        // it is a cache, not custody.
        *self
            .provision_reach_ipv4
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        // Same for the injected-seed identity root: it is keyed by domain and a
        // "start over" may pick a different one. The next run re-holds it —
        // from `pending_provision_row` when it resumes a box an earlier run
        // built, else from its own fresh seed.
        //
        // NOT `pending_provision_row`: the slot survives a reset (§ 6 — "only
        // a completed claim or the surface's explicit exit" clears it, since
        // the box still exists and still bills), and so must the machine's
        // memory of it, or a re-provision of the same domain would mint a
        // fresh code and identity for a box that boots with the old ones —
        // the very poisoning Retry used to inflict. Pinned by
        // `tests/pending_provision_slot.rs`.
        *self
            .nest_expected_identity
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.observer.on_changed();
    }

    pub fn step(&self) -> OnboardingStep {
        self.with_state(|s| s.step)
    }

    pub fn current_handle(&self) -> String {
        self.with_state(|s| s.handle.clone())
    }

    pub fn set_current_handle(&self, h: String) {
        self.mutate(|s| s.handle = h);
    }

    /// Apply a **nest hint** to the handle-entry page: pre-fill the handle's
    /// domain part with `raw` when it classifies as a nest target, and report
    /// whether it did (`onboarding.md` § 2 Handle entry → *Nest hint*).
    ///
    /// On web the hint is the `nest` query parameter a nest's central-origin
    /// redirect carries (`web-content-hosting.md` § The nest-served `/app/`
    /// and the central origin); a native deep link can hand the same raw value
    /// here later. Parsing and the drop rule live HERE so every app applies the
    /// same rule and none reads the raw hint into its own state:
    ///
    /// - The hint must name exactly one `host[:port]` authority
    ///   (`fauna_core::web::is_domain_authority_syntax` — no userinfo, path,
    ///   query or whitespace) before the shared probe classifier
    ///   (`resolve_handle_domain`) is trusted with it: that classifier is a
    ///   negative test and would call `evil.example/x?y` "public".
    /// - A hint that does not classify is **dropped silently** — the field is
    ///   left as it was and `false` comes back; never an error message.
    /// - It is a prefill and nothing more: any local part already typed is
    ///   kept, the step does not change, and the check, probe and trust
    ///   ceremony run exactly as for a typed domain. App-launch routing runs
    ///   before onboarding and never consults the hint, so a signed-in app
    ///   never reaches this call.
    pub fn set_nest_hint(&self, raw: String) -> bool {
        let hint = raw.trim();
        if !fauna_core::web::is_domain_authority_syntax(hint) {
            return false;
        }
        if fauna_provisioning::probe::resolve_handle_domain(hint)
            .base_url
            .is_empty()
        {
            return false;
        }
        self.mutate(|s| {
            let local = s
                .handle
                .split_once('@')
                .map_or(s.handle.as_str(), |(l, _)| l)
                .trim()
                .to_string();
            s.handle = format!("{local}@{hint}");
        });
        true
    }

    /// Set (or clear) the age claim the next admission carries
    /// (`family-safety.md` § The account age band). The mobile apps call this
    /// with their store age signal — attested when the platform attestation
    /// round succeeded, declared-only otherwise; every other app never calls
    /// it. Both `submit_invite_request` and `register` carry it through
    /// [`Self::age_claim_to_send`], which strips an attestation the addressed
    /// nest did not say it can check.
    pub fn set_age_claim(&self, claim: Option<AgeClaimPlain>) {
        *self.age_claim.write().unwrap_or_else(|e| e.into_inner()) = claim.map(Into::into);
    }

    /// The age claim the glue set, if any — verbatim, attestation included
    /// (the iOS round reads it to decide a re-mint). What actually rides the
    /// wire is [`Self::age_claim_to_send`].
    pub fn age_claim(&self) -> Option<AgeClaimPlain> {
        self.age_claim
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(Into::into)
    }

    /// Mint the single-use nonce the platform attestation binds
    /// (`fauna.account.age_nonce`, pre-identity, against the wizard's current
    /// nest). The app then runs its attestation round over
    /// [`Self::age_claim_message`] inside `expires_in_secs` and hands the
    /// result to [`Self::set_age_claim`]. Minted from the nest the admission
    /// will go to (`effective_nest_url`, the URL both admission paths use), and
    /// remembered — with the platforms the reply listed — for the send guard.
    pub async fn request_age_nonce(&self) -> Result<AgeNoncePlain, OnboardingError> {
        let nest_url = self.effective_nest_url();
        match self.nest_api.age_nonce(&nest_url).await {
            Ok(n) => {
                *self
                    .age_nonce_mint
                    .write()
                    .unwrap_or_else(|e| e.into_inner()) = Some(AgeNonceMint {
                    nest_url,
                    nonce_hex: n.nonce_hex.clone(),
                    attestation_platforms: n.attestation_platforms.clone(),
                });
                Ok(AgeNoncePlain {
                    nonce_hex: n.nonce_hex,
                    expires_in_secs: n.expires_in_secs,
                    attestation_platforms: n.attestation_platforms,
                })
            }
            Err(crate::nest_api::AgeNonceError::Transient { cause }) => {
                Err(OnboardingError::Network { detail: cause })
            }
            Err(crate::nest_api::AgeNonceError::Refused { reason }) => {
                Err(OnboardingError::Other { detail: reason })
            }
            // The typed identity verdict (security.md § Pre-claim surfacing):
            // terminal, not the retryable Network bucket.
            Err(crate::nest_api::AgeNonceError::IdentityMismatch { reason }) => {
                Err(OnboardingError::Other { detail: reason })
            }
        }
    }

    /// The exact bytes the platform attestation must commit to —
    /// `fauna_protocol::age::age_claim_signed_message(nonce, band,
    /// application_id, actor_id)` for the wizard's current identity. What
    /// the platforms actually consume is their SHA-256,
    /// [`Self::age_claim_digest`]; this is the pre-image, for transparency
    /// and tests. One definition, shared with the nest's verifier.
    pub fn age_claim_message(
        &self,
        nonce_hex: String,
        band: String,
        application_id: String,
    ) -> Result<Vec<u8>, OnboardingError> {
        let secret = self
            .effective_secret()
            .ok_or_else(|| OnboardingError::InvalidTransition {
                from: format!("{:?}", self.step()),
                reason: "no identity to bind the age claim to yet".into(),
            })?;
        let signing_key = parse_signing_key(&secret).ok_or_else(|| OnboardingError::Other {
            detail: "invalid identity secret".into(),
        })?;
        let nonce = fauna_core::hex32::decode(&nonce_hex).map_err(|_| OnboardingError::Other {
            detail: "malformed age nonce".into(),
        })?;
        Ok(fauna_protocol::age::age_claim_signed_message(
            &nonce,
            &band,
            &application_id,
            &signing_key.verifying_key().to_bytes(),
        ))
    }

    /// SHA-256 of [`Self::age_claim_message`] — the ONE value both platform
    /// attestations bind: iOS hands it to App Attest as `clientDataHash`;
    /// android passes it base64url-unpadded as the Play Integrity classic
    /// request's nonce. A digest on purpose: Apple's and Google's logs see
    /// 32 opaque bytes, never the band or the actor id in the pre-image. The
    /// nest recomputes exactly this on both arms.
    pub fn age_claim_digest(
        &self,
        nonce_hex: String,
        band: String,
        application_id: String,
    ) -> Result<Vec<u8>, OnboardingError> {
        use sha2::Digest as _;
        let message = self.age_claim_message(nonce_hex, band, application_id)?;
        Ok(sha2::Sha256::digest(&message).to_vec())
    }

    pub fn error_message(&self) -> Option<String> {
        self.with_state(|s| s.error_message.clone())
    }

    pub fn clear_error(&self) {
        self.mutate(|s| s.error_message = None);
    }

    /// Put a client-resolved message on the shared `error-message` channel.
    ///
    /// The counterpart of [`Self::begin_import_identity_with_reason`]'s
    /// `reason`, without the transition: the machine holds no string table, so
    /// a surface whose outcome is a typed value (today
    /// [`RecoveryEntryOutcome`]) resolves the `onboarding.*` key its client-side
    /// string table owns and stores the result here, where every app's
    /// `error-message` already reads from. Keeping it on the machine rather
    /// than in per-app view state is what makes the message survive the
    /// observer tick that fires with it (the same reasoning
    /// `begin_import_identity_with_reason` documents).
    pub fn set_error_message(&self, message: String) {
        self.mutate(|s| s.set_error(message));
    }

    pub fn is_loading(&self) -> bool {
        self.with_state(|s| s.is_loading)
    }

    // ── Identity stage ──────────────────────────────────────────────────

    pub fn generated_secret(&self) -> Option<String> {
        self.with_state(|s| s.generated_secret.clone().map(String::from))
    }

    /// The identity the wizard is acting as — the secret every authenticating
    /// call signs with, and the one the app's wizard terminal persists
    /// (`onboarding.md` § Long-term store contract). `None` until the user has
    /// committed an identity. The rule (origin decides, other slot as fallback)
    /// and why it is a rule rather than a fixed precedence: [`State::canonical_secret`].
    pub fn effective_secret(&self) -> Option<String> {
        self.with_state(|s| s.canonical_secret().map(String::from))
    }

    pub fn identity_origin(&self) -> Option<IdentityOrigin> {
        self.with_state(|s| s.identity_origin)
    }

    /// Enter `identity_created`. Entry **mints, and does not commit.**
    ///
    /// The origin is deliberately *not* written here. [`State::canonical_secret`]
    /// reads it as *"the screen the user committed on"* (`onboarding.md`
    /// § 1 Identity), and this screen's entry fills its own slot — so an
    /// entry-time write would make mere curiosity outrank an identity the user
    /// already pasted and confirmed: import the real key, tap "create" to look
    /// around, go Back, and every authenticating call signs as the throwaway
    /// while the wizard terminal persists it (§ Long-term store contract).
    /// [`Self::begin_import_identity`] *can* afford the entry-time write because
    /// it leaves its slot empty, so "origin says `Imported`, slot is empty" is a
    /// legible back-out state the fallback arm covers; a minted-on-entry slot
    /// has no such tell. The commit point is [`Self::confirm_generated_identity`].
    ///
    /// The mint is guarded for the same reason the recovery-kit root is
    /// (see [`Self::confirm_generated_identity`]): re-entry after
    /// back-navigation must re-show the SAME key, or a key the user was just
    /// told to write down is silently invalidated.
    pub fn begin_create_identity(&self) {
        self.mutate(|s| {
            Self::clear_recovery(s);
            if s.generated_secret.is_none() {
                s.generated_secret = Some(fauna_provisioning::generate_keypair_hex().into());
            }
            s.step = OnboardingStep::IdentityCreated;
        });
    }

    pub fn begin_import_identity(&self) {
        self.mutate(|s| {
            Self::clear_recovery(s);
            s.identity_origin = Some(IdentityOrigin::Imported);
            s.step = OnboardingStep::IdentityImport;
        });
    }

    /// [`Self::begin_import_identity`], carrying the reason the user was *sent*
    /// there — for arrivals the user did not ask for.
    ///
    /// The launch flow's `superseded` refusal is the case that needs it
    /// (`identity-succession.md` § Propagation → *Own device fleet*: the client
    /// "surfaces 'this identity was succeeded — import the new identity'"): the
    /// affordance IS the import screen, so the only thing distinguishing it from
    /// a user who chose to import is the explanation on that page's existing
    /// `error-message`. `recovery_entry`'s own superseded refusal
    /// (`onboarding.md` § 1 Identity) routes the same way.
    ///
    /// **Why the reason lives in machine state rather than at the call site.**
    /// The per-app onboarding views mirror `error_message()` reactively on every
    /// observer tick (linux's `handle_change` re-reads it into the GTK label each
    /// time), so a reason written straight to a widget is erased by the next tick
    /// — including the tick this very transition fires. Setting both fields under
    /// one [`Self::mutate`] also makes the pair atomic: no observer ever sees the
    /// import step without the explanation that justifies it.
    pub fn begin_import_identity_with_reason(&self, reason: String) {
        self.mutate(|s| {
            Self::clear_recovery(s);
            s.identity_origin = Some(IdentityOrigin::Imported);
            s.step = OnboardingStep::IdentityImport;
            s.set_error(reason);
        });
    }

    pub fn confirm_generated_identity(&self) -> Result<String, OnboardingError> {
        self.reset_handle_check();
        self.mutate(|s| {
            let secret =
                s.generated_secret
                    .clone()
                    .ok_or_else(|| OnboardingError::InvalidTransition {
                        from: "identity_created".into(),
                        reason: "generated_secret missing".into(),
                    })?;
            // The commit point. `identity_created` only becomes "the screen the
            // user committed on" here, not at entry — see
            // [`Self::begin_create_identity`] for why the door is the wrong
            // place to write it.
            s.identity_origin = Some(IdentityOrigin::Created);
            if s.renders_recovery_kit {
                // The kit offer comes right after the identity secret
                // (onboarding.md § 1 Identity). Mint only if no pending root
                // exists — re-entry after back-navigation must re-show the
                // SAME phrase, or a phrase the user already wrote down is
                // silently invalidated.
                if s.pending_recovery_secret.is_none() {
                    s.pending_recovery_secret =
                        Some(fauna_provisioning::generate_keypair_hex().into());
                }
                s.step = OnboardingStep::RecoveryKit;
            } else {
                s.step = OnboardingStep::HandleEntry;
            }
            Ok(secret.into())
        })
    }

    /// App capability declaration: this app has the `recovery_kit` onboarding
    /// screen built (`onboarding.md` § 1 Identity). Call once at wizard
    /// construction; the machine then routes `confirm_generated_identity`
    /// through the kit screen. Apps that never call it keep the pre-existing
    /// straight-to-`HandleEntry` flow — the batched-trickle-down parity gap.
    pub fn set_renders_recovery_kit(&self, renders: bool) {
        self.mutate(|s| s.renders_recovery_kit = renders);
    }

    /// The minted-but-unregistered RecoveryKey root the `recovery_kit` screen
    /// displays (64-hex + QR). `None` outside the kit screen's lifetime.
    /// Hands out a plain `String` at the render boundary (the foreign string
    /// is un-zeroizable by design — `fauna_core::secret` module docs); the
    /// held copy stays the zeroizing, `Debug`-redacted [`SecretString`].
    pub fn recovery_kit_secret_hex(&self) -> Option<String> {
        self.with_state(|s| {
            s.pending_recovery_secret
                .as_ref()
                .map(|sec| sec.as_str().to_owned())
        })
    }

    /// The `fauna://recovery` URI behind the `recovery_kit` screen's QR **and**
    /// its copy button — one payload for both (`identity-succession.md` § The
    /// RecoveryKey, *Which encoding each affordance carries*); the display
    /// alone is the bare [`Self::recovery_kit_secret_hex`]. It names the
    /// account (the actor the generated identity derives to) and truthfully no
    /// handle — none is chosen yet at this position, so a restore from it asks
    /// (`recovery-entry-account-field`). `None` outside the screen's lifetime.
    /// Built here so every app renders the same payload (priority #2).
    pub fn recovery_kit_uri(&self) -> Option<String> {
        let kit_hex = self.recovery_kit_secret_hex()?;
        let actor_hex = self
            .generated_secret()
            .and_then(|s| fauna_core::identity::ActorKeypair::from_secret_hex(&s).ok())
            .map(|kp| kp.actor_id_hex());
        Some(fauna_core::recovery::RecoveryKitQr::to_uri(
            &kit_hex,
            actor_hex.as_deref(),
            None,
        ))
    }

    /// `recovery-kit-confirm-button`: the user says the phrase is saved.
    /// Advances to `HandleEntry`; the pending root is KEPT for the signed-in
    /// handoff, where the per-app glue takes it
    /// ([`Self::take_pending_recovery_secret`]) and runs registration + escrow.
    pub fn confirm_recovery_kit(&self) {
        self.reset_handle_check();
        self.mutate(|s| {
            s.error_message = None;
            s.step = OnboardingStep::HandleEntry;
        });
    }

    /// `recovery-kit-skip-button`: decline the kit. One click, never blocks
    /// onboarding; the minted root is dropped (zeroized) so nothing registers
    /// at handoff, and Settings' never-created warning tells the truth.
    pub fn skip_recovery_kit(&self) {
        self.reset_handle_check();
        self.mutate(|s| {
            s.pending_recovery_secret = None;
            s.error_message = None;
            s.step = OnboardingStep::HandleEntry;
        });
    }

    /// Consume the pending RecoveryKey root at the wizard's signed-in handoff —
    /// the one point custody permits registration (the root is never
    /// persisted, so it cannot survive to a later session). Returns `None` if
    /// the kit was skipped, already taken, or never offered.
    pub fn take_pending_recovery_secret(&self) -> Option<SecretString> {
        self.mutate(|s| s.pending_recovery_secret.take())
    }

    /// `restore-from-recovery-kit-button` on `identity_choice` — the
    /// phrase-only IDENTITY restore (`onboarding.md` § 1 Identity). Routes to
    /// the `recovery_entry` screen. Distinct from `begin_recover_lost_box`,
    /// which starts the total-box-loss NEST recovery branch.
    pub fn begin_recovery_entry(&self) {
        self.mutate(|s| {
            Self::clear_recovery(s);
            s.error_message = None;
            s.step = OnboardingStep::RecoveryEntry;
        });
    }

    /// `recovery-entry-submit-button` — the phrase-only identity restore
    /// (`onboarding.md` § 1 Identity; ceremony
    /// `identity-succession.md` § Seed escrow → *Restore path*).
    ///
    /// `kit_input` is `recovery-entry-phrase-field` verbatim (the
    /// `fauna://recovery` URI or a bare 64-hex code). The account comes from
    /// [`Self::current_handle`] — the wizard's one account field, which the
    /// client fills from `recovery-entry-account-field` when the user typed
    /// one; when it is empty the kit payload's own `handle=` supplies it, and
    /// this method writes that back so `handle_entry` pre-fills exactly as an
    /// `identity_import` QR's handle does.
    ///
    /// On success the seed is restored and the wizard lands on `HandleEntry` —
    /// deliberately the same landing `confirm_imported_identity` produces: the
    /// seed *is* recovered, so everything downstream is an import.
    ///
    /// Returns the outcome rather than a message because the machine holds no
    /// string table; the client resolves the i18n key
    /// (`onboarding.recovery_entry.*`) and renders it on `error-message`, the
    /// same division the handle-check's `LocalizedText` snapshots use.
    /// [`RecoveryEntryOutcome::Superseded`] is the one variant that does not
    /// stay on the page: the client routes it to
    /// [`Self::begin_import_identity_with_reason`], uniform with the launch
    /// flow's superseded refusal.
    pub async fn submit_recovery_entry(&self, kit_input: String) -> RecoveryEntryOutcome {
        // Clear first: a message left over from the previous attempt must never
        // be readable as this one's answer while the ceremony is in flight.
        self.clear_error();

        let Ok(kit) = fauna_client_recovery::parse_kit(&kit_input) else {
            return self.fail_recovery_entry(RecoveryEntryOutcome::InvalidKit);
        };

        // The typed field wins over the payload: a user who names an account
        // is correcting or supplying what the phrase could not say.
        let typed = self.current_handle();
        let typed = typed.trim();
        let account = if typed.is_empty() {
            kit.handle.clone()
        } else {
            Some(typed.to_string())
        };
        let Some(account) = account else {
            return self.fail_recovery_entry(RecoveryEntryOutcome::AccountNeeded);
        };
        // Only a handle's `@domain` can locate a nest, and the ceremony is
        // pre-identity — there is no session to ask. A bare local part is
        // therefore not enough here, unlike at `handle_entry` where a local
        // nest may already be in hand.
        let Some(domain) = parse_handle_domain(&account) else {
            return self.fail_recovery_entry(RecoveryEntryOutcome::AccountMalformed);
        };

        let base_url = self.resolve_restore_target(&domain).await;
        let actor_hex = kit.actor_id.map(|a| hex::encode(a.0));
        let outcome = self
            .nest_api
            .restore_escrowed_seed(
                &base_url,
                &kit.recovery.to_hex(),
                actor_hex.as_deref(),
                // The nest stores handles without the `@domain` suffix — the
                // same trap the register path shipped once.
                Some(handle_local_part(&account)),
            )
            .await;

        match outcome {
            Ok(restored) => {
                self.reset_handle_check();
                // A restore run inside a succession's corpus re-seal window
                // brings back predecessor seeds too. They are parked beside the
                // identity seed for the client to persist into its account
                // registry (`identity-succession.md` § Seed escrow) — this
                // machine has no store of its own, exactly as it has none for
                // `imported_secret`.
                let unreadable = restored.predecessors_unreadable.clone();
                self.mutate(|s| {
                    s.imported_secret = Some(restored.seed_hex.into());
                    s.restored_predecessors = restored.predecessors;
                    s.restored_predecessors_unreadable = restored.predecessors_unreadable;
                    // Carry the account forward: it is what `handle_entry`
                    // asks for next, and the user just proved they own it.
                    s.handle = account;
                    s.identity_origin = Some(IdentityOrigin::Imported);
                    s.step = OnboardingStep::HandleEntry;
                    s.error_message = None;
                });
                // The account IS back — this is never a refusal. But a broken
                // predecessor section means a corpus still sealed under a
                // predecessor just became unopenable, so the outcome carries
                // the fact rather than letting `Restored` imply completeness.
                match unreadable {
                    Some(reason) => RecoveryEntryOutcome::RestoredPredecessorsLost { reason },
                    None => RecoveryEntryOutcome::Restored,
                }
            }
            Err(e) => self.fail_recovery_entry(match e {
                RestoreSeedError::NoEscrow => RecoveryEntryOutcome::NoEscrow,
                // Not carried into the outcome: the successor arrives
                // unverified, and this client does not repeat an unproven
                // claim (`identity-succession.md` § Propagation — verify the
                // claim, don't trust it).
                RestoreSeedError::Superseded { .. } => RecoveryEntryOutcome::Superseded,
                RestoreSeedError::AccountUnknown { handle } => {
                    RecoveryEntryOutcome::AccountUnknown { account: handle }
                }
                RestoreSeedError::AccountUnnamed => RecoveryEntryOutcome::AccountNeeded,
                RestoreSeedError::Refused { reason } => RecoveryEntryOutcome::Refused { reason },
                RestoreSeedError::Transient { reason } => {
                    RecoveryEntryOutcome::Unreachable { reason }
                }
                // The typed identity verdict (security.md § Pre-claim
                // surfacing): the server reached is not the expected nest.
                // Terminal like a refusal — NOT `Unreachable`, whose screen
                // invites an as-is retry that would re-earn the verdict.
                RestoreSeedError::IdentityMismatch { reason } => {
                    RecoveryEntryOutcome::Refused { reason }
                }
            }),
        }
    }

    /// Log a refused restore and hand the outcome back unchanged.
    ///
    /// The step is left alone on purpose: every refusal here is something the
    /// user acts on *from this screen* (fix the account, paste a different
    /// phrase, retry) — except `Superseded`, whose routing the client owns.
    ///
    /// It deliberately does **not** write `error_message`. The outcome is the
    /// contract, and the message that renders is the client's localized
    /// resolution of it ([`Self::set_error_message`]) — writing an English
    /// placeholder here would put a second, untranslated owner on the one
    /// channel `error-message` reads.
    fn fail_recovery_entry(&self, outcome: RecoveryEntryOutcome) -> RecoveryEntryOutcome {
        tracing::warn!(target: "fauna_onboarding", "recovery entry refused: {outcome:?}");
        outcome
    }

    /// The nest URL a restore connects to, resolved from the account handle's
    /// domain exactly as the handle-check resolves its probe target: the local
    /// / loopback classification first, then SRV port discovery for a public
    /// name that carried no explicit port, so a clean `alice@domain` stays
    /// port-hidden.
    ///
    /// The `"nest"` provider override wins when set, the same way
    /// `run_handle_check_phases` lets it win over `target.base_url`. It has to
    /// be resolved **here**, not left to `WsNestApi`: that type captures the
    /// map once at construction, while the E2E bridge installs the override at
    /// runtime through `set_provider_base_urls`, so a machine-side caller that
    /// skips this step dials the resolved `https://` URL at a harness nest
    /// serving plain HTTP (which surfaces as a corrupt-message WS handshake —
    /// the failure this method's own tier_3 journey caught).
    async fn resolve_restore_target(&self, domain: &str) -> String {
        let mut target = fauna_provisioning::probe::resolve_handle_domain_with_local_port(
            domain,
            *self
                .local_nest_port
                .read()
                .unwrap_or_else(|e| e.into_inner()),
        );
        if let Some(override_url) = self.provider_base_url("nest".into()) {
            return override_url;
        }
        if target.is_public_dns_name {
            let (host, typed_port) = fauna_core::resolve::parse_node_address(&target.base_url);
            if typed_port.is_none()
                && let Some(srv_port) =
                    fauna_provisioning::probe::fauna_srv_port(&self.http, &host).await
                && srv_port != 443
            {
                target.base_url = format!("https://{host}:{srv_port}");
            }
        }
        target.base_url
    }

    pub fn confirm_imported_identity(&self, secret: String) -> Result<String, OnboardingError> {
        if !is_hex64(&secret) {
            // Surface the validation error via state.error_message so the
            // page's error-banner renders the message — the per-app
            // import view can use `try?` and lean on the observer to
            // re-render the banner without writing platform-specific
            // error-handling glue. Same pattern as `submit_handle`.
            self.mutate(|s| {
                s.set_error("Secret key must be exactly 64 hexadecimal characters".into());
            });
            return Err(OnboardingError::InvalidTransition {
                from: "identity_import".into(),
                reason: "secret must be 64 hex chars".into(),
            });
        }
        self.reset_handle_check();
        self.mutate(|s| {
            s.imported_secret = Some(secret.clone().into());
            // Recovery intent (came from identity_choice's recover-lost-box-button)
            // now lands on handle_entry too (Q2-A, box-recovery.md § Recovery UI):
            // the admin still needs to connect to a SURVIVING nest before the
            // box list is readable, so recovery routes through the same
            // nest-connect / handle-`@domain` step as a normal import.
            // `submit_handle_check_continue`'s recovery-intent arm is what lands
            // on `NestRecovery` once that nest resolves as `AlreadyOnNest`.
            s.step = OnboardingStep::HandleEntry;
            s.error_message = None;
        });
        Ok(secret)
    }

    /// Pre-seed the wizard with an identity loaded from the client's
    /// long-term store at app launch. Skips identity-creation/import and
    /// places the wizard at HandleEntry. No validation — the caller is
    /// responsible for providing a valid 64-char hex secret it owns.
    ///
    /// `identity_origin` is left `None` (not `Imported`) so that pressing
    /// Back from `HandleEntry` routes to `IdentityChoice` — the seeded
    /// user never visited `IdentityImport`/`IdentityCreated` in this
    /// session, so routing them there would show an empty paste form they
    /// can't act on. From `IdentityChoice` they can re-pick if they want
    /// to overwrite the seeded identity.
    pub fn seed_identity(&self, secret: String) {
        self.mutate(|s| {
            s.imported_secret = Some(secret.into());
            s.identity_origin = None;
            s.step = OnboardingStep::HandleEntry;
        });
    }

    // ── Handle entry stage ──────────────────────────────────────────────

    pub fn local_nest_reachable(&self) -> bool {
        self.with_state(|s| s.local_nest_reachable)
    }

    /// Derived view over `handle_check_snapshot().outcome` — NOT stored
    /// state. `DomainAvailable` ⟺ `Unregistered`, `RegisteredNoNest` ⟺
    /// itself; every other outcome (incl. `None` after a reset) has no
    /// domain-registration verdict to report. Sound because the outcome and
    /// this view share exactly one reset point (`reset_handle_check`, fired
    /// on identity change) and the outcome is otherwise stable for the
    /// lifetime of the DnsConfig step (`submit_handle_check_continue` reads
    /// it once to decide the transition and never mutates it further).
    pub fn domain_status(&self) -> Option<fauna_provisioning::probe::DomainStatus> {
        use crate::snapshots::HandleCheckOutcome;
        use fauna_provisioning::probe::DomainStatus;
        match self.handle_check.lock().unwrap().outcome {
            HandleCheckOutcome::DomainAvailable { .. } => Some(DomainStatus::Unregistered),
            HandleCheckOutcome::RegisteredNoNest => Some(DomainStatus::RegisteredNoNest),
            _ => None,
        }
    }

    // ── Nest URL setter (used by seed/test paths) ───────────────────────

    pub fn nest_url(&self) -> String {
        self.with_state(|s| s.nest_url.clone())
    }

    /// Pre-identity probe used by **app-launch routing** to discriminate
    /// "secret unregistered + claimed nest → invite_request" from
    /// "secret unregistered + unclaimed nest → claim_code" (per
    /// `docs/goal/behavior/onboarding.md` § App-launch routing).
    ///
    /// Delegates to the active `NestApi`, so this rides the anonymous WS-RPC
    /// connection just like the rest of the wizard — replacing the legacy
    /// `GET /api/v1/setup-status` HTTP probe the web / launch-machine glue
    /// used to call directly. The orchestrator
    /// applies the safer-default fallback (`Err` → assume `claimed=true`);
    /// surfacing the raw error here keeps that decision in one place per
    /// caller rather than baking it into the machine.
    pub async fn probe_setup_status_at(
        &self,
        nest_url: String,
    ) -> Result<crate::nest_api::SetupStatus, crate::nest_api::ProbeError> {
        self.nest_api.probe_setup_status(&nest_url).await
    }

    pub fn set_nest_url(&self, url: String) {
        self.mutate(|s| s.nest_url = url);
    }

    // ── DNS config stage ────────────────────────────────────────────────

    pub fn dns_config(&self) -> DnsConfigState {
        self.with_state(|s| s.dns.clone())
    }

    pub fn toggle_buy_domain(&self, on: bool) {
        self.mutate(|s| {
            s.dns.buy_domain = on;
            // Deselect provider if it can't satisfy the new constraints.
            if on
                && let Some(pid) = s.dns.selected_provider_id.as_deref()
                && let Some(p) = PROVIDERS.iter().find(|p| p.id.as_str() == pid)
                && !p.capabilities.contains(&Capability::Registrar)
            {
                s.dns.selected_provider_id = None;
                s.dns.creds.clear();
                s.dns.verified = false;
                s.dns.current_zones.clear();
                s.dns.current_availability = None;
                s.dns.contact = None;
            }
        });
    }

    pub fn toggle_same_provider_for_vps(&self, on: bool) {
        self.mutate(|s| {
            s.dns.same_provider_for_vps = on;
            if on
                && let Some(pid) = s.dns.selected_provider_id.as_deref()
                && let Some(p) = PROVIDERS.iter().find(|p| p.id.as_str() == pid)
                && !p.capabilities.contains(&Capability::Vps)
            {
                s.dns.selected_provider_id = None;
                s.dns.creds.clear();
                s.dns.verified = false;
                s.dns.zone_id = None;
                s.dns.current_zones.clear();
                s.dns.current_availability = None;
                s.dns.contact = None;
            }
        });
    }

    pub fn select_dns_provider(&self, id: String) {
        self.mutate(|s| {
            s.dns.selected_provider_id = Some(id);
            s.dns.creds.clear();
            s.dns.hosted_auth.clear();
            s.dns.verified = false;
            s.dns.zone_id = None;
            s.dns.current_zones.clear();
            s.dns.current_availability = None;
            s.dns.contact = None;
        });
    }

    pub fn set_dns_cred(&self, field_id: String, value: String) {
        self.mutate(|s| {
            // Wrap at the input boundary — the plain `String` arrived across
            // FFI/WASM/GTK; from here it lives as a zeroize-on-drop secret.
            s.dns.creds.insert(field_id, SecretString::from(value));
            s.dns.verified = false;
        });
    }

    pub fn dns_set_up_later(&self) {
        self.mutate(|s| {
            s.dns.set_up_later = true;
            s.step = OnboardingStep::VpsConfig;
        });
    }

    /// Records the user's explicit acceptance of the displayed registration
    /// price. Required before `start_provisioning()` will run the buy-domain
    /// path. Pair with `dns_status_text()` (or its i18n key variant) so the
    /// UI shows the user the exact price before they consent.
    pub fn confirm_price(&self) {
        self.mutate(|s| s.dns.price_agreed = true);
    }

    /// Sets the WHOIS contact for the buy-domain path. Called by the per-
    /// client UI's contact form on submit. Replaces any contact previously
    /// prefilled by `verify_dns` via `Registrar::fetch_default_contact()`.
    pub fn set_contact(&self, contact: fauna_provisioning::registrar::ContactInfo) {
        self.mutate(|s| s.dns.contact = Some(contact));
    }

    /// Fields visible on the dns_config form. Filters by `FieldMeta::kinds`:
    /// always include kinds containing Dns; additionally include Vps-kinded
    /// fields when same_provider_for_vps is on AND the provider has Vps cap,
    /// and Registrar-kinded fields when buy_domain is on AND the provider has
    /// Registrar cap (e.g. cloudflare's `account-id`, `kinds: [registrar]` —
    /// without this arm a registrar-only-kinded field never renders, so its
    /// credential is never collected and the registrar API call it feeds
    /// fails downstream instead of at the form).
    pub fn visible_dns_fields(&self) -> Vec<FieldMetaPlain> {
        self.with_state(|s| {
            let pid = match s.dns.selected_provider_id.as_deref() {
                Some(p) => p,
                None => return vec![],
            };
            let p = match PROVIDERS.iter().find(|p| p.id.as_str() == pid) {
                Some(p) => p,
                None => return vec![],
            };
            let show_vps = s.dns.same_provider_for_vps && p.capabilities.contains(&Capability::Vps);
            let show_registrar =
                s.dns.buy_domain && p.capabilities.contains(&Capability::Registrar);
            p.fields
                .iter()
                .filter(|f| {
                    f.kinds.contains(&Capability::Dns)
                        || (show_vps && f.kinds.contains(&Capability::Vps))
                        || (show_registrar && f.kinds.contains(&Capability::Registrar))
                })
                .map(FieldMetaPlain::from)
                .collect()
        })
    }

    pub fn can_verify_dns(&self) -> bool {
        let visible = self.visible_dns_fields();
        self.with_state(|s| {
            !s.dns.verifying
                && visible.iter().all(|f| {
                    !f.required
                        || s.dns
                            .creds
                            .get(&f.id)
                            .map(|v| !v.is_empty())
                            .unwrap_or(false)
                })
        })
    }

    pub async fn verify_dns(&self) -> Result<(), OnboardingError> {
        use fauna_provisioning::dispatch;
        use fauna_provisioning::dns::DnsProvider;
        use fauna_provisioning::probe::DomainStatus;
        use fauna_provisioning::registrar::Registrar;

        // Mark verifying up front so `can_verify_dns()` returns false from
        // here on; clients gate their verify-button on that predicate so
        // the button stays disabled across all 6 platforms while the
        // probe is in flight. The early `?` returns below skip the
        // mutation block at the bottom — they all early-return BEFORE
        // any await, so leaving verifying=false on those paths is fine.
        self.mutate(|s| s.dns.verifying = true);
        // RAII-ish reset closure: every Err return path between here and
        // success runs a clear so we don't strand verifying=true if a
        // transient failure happens.
        let clear_verifying = |m: &Self| m.mutate(|s| s.dns.verifying = false);

        let snap = self.snapshot();
        let pid = match snap.dns.selected_provider_id.clone() {
            Some(p) => p,
            None => {
                clear_verifying(self);
                return Err(OnboardingError::InvalidTransition {
                    from: "dns_config".into(),
                    reason: "no DNS provider selected".into(),
                });
            }
        };
        let creds = snap.dns.creds.clone();
        let handle_domain = snap.handle_domain().map(str::to_string);
        let domain_status = self.domain_status();
        let existing_contact = snap.dns.contact.clone();

        let provider_id = match dispatch_provider_id(&pid) {
            Some(p) => p,
            None => {
                clear_verifying(self);
                return Err(OnboardingError::Other {
                    detail: format!("unknown provider id: {pid}"),
                });
            }
        };

        let dns_dispatch = match dispatch::dns_provider(
            provider_id,
            dispatch::Credentials::from_map(creds.clone()),
            // Honor the `provider_base_urls["dns"]` E2E override so the
            // verify probe hits the fake instead of the live DNS API —
            // the same redirection the orchestrator applies (see
            // `run_provisioning_inner`). `None` in production.
            self.provider_base_url("dns".into()),
        ) {
            Some(d) => d,
            None => {
                clear_verifying(self);
                return Err(OnboardingError::Other {
                    detail: format!("provider {pid} doesn't support DNS"),
                });
            }
        };

        let zones = match dns_dispatch.verify(&self.http).await {
            Ok(zones) => zones,
            Err(e) => {
                let msg = e.to_string();
                self.mutate(|s| {
                    s.set_error(format!("Verify failed: {msg}"));
                    s.dns.verifying = false;
                });
                return Err(OnboardingError::ProviderUnauthorized { provider: pid });
            }
        };

        // Optional registrar-driven enrichment. Failures here are non-
        // fatal: provider_status() handles missing data correctly.
        let mut current_availability = None;
        let mut prefill_contact = None;
        let registrar_dispatch =
            dispatch::registrar(provider_id, dispatch::Credentials::from_map(creds.clone()));
        if let Some(reg) = registrar_dispatch.as_ref() {
            // Eager availability when domain isn't already registered.
            if matches!(domain_status, Some(DomainStatus::Unregistered))
                && let Some(domain) = handle_domain.as_deref()
                && let Ok(av) = reg.availability(&self.http, domain).await
            {
                current_availability = Some(av);
            }
            // Err: leave None; provider_status returns
            // UnregisteredNotBuyable.
            // Prefill contact if registrar accepts per-registration
            // contacts AND we don't already have one set.
            if reg.requires_contact()
                && existing_contact.is_none()
                && let Ok(Some(c)) = reg.fetch_default_contact(&self.http).await
            {
                prefill_contact = Some(c);
            }
            // Err / Ok(None): leave existing_contact unchanged
            // (the wizard form starts empty).
        }

        // Pick the zone that is, or contains, the handle's domain — the same
        // rule `provider_status` reads (`covering_zone`); fall back to the
        // first zone only when none covers it.
        let zone_id = handle_domain
            .as_deref()
            .and_then(|d| covering_zone(d, &zones).map(|z| z.id.clone()))
            .or_else(|| zones.first().map(|z| z.id.clone()));

        self.mutate(|s| {
            s.dns.verified = true;
            s.dns.zone_id = zone_id;
            s.dns.current_zones = zones;
            s.dns.current_availability = current_availability;
            if let Some(c) = prefill_contact {
                s.dns.contact = Some(c);
            }
            s.error_message = None;
            s.dns.verifying = false;
        });
        Ok(())
    }

    /// Per-provider DNS-config status. Pure computation over the current
    /// snapshot. UIs consume this via WASM/UniFFI and switch UI shape on
    /// the variant; `can_continue_dns` consults it to decide whether
    /// Continue is enabled.
    pub fn provider_status(&self) -> crate::state::ProviderStatus {
        use crate::state::ProviderStatus;
        use fauna_provisioning::probe::DomainStatus;
        use fauna_provisioning::registrar::RegistrarAvailability;

        let domain_status = self.domain_status();
        self.with_state(|s| {
            // NotReady: no provider selected or verify hasn't completed.
            if s.dns.selected_provider_id.is_none() || !s.dns.verified {
                return ProviderStatus::NotReady;
            }

            // ProviderHasDomain: a zone in current_zones IS the handle's
            // domain or CONTAINS it (`example.com` for `box.example.com`) —
            // the provider can publish every record the box needs inside that
            // zone, which is all this status asserts
            // (`onboarding-provisioning.md` § 4).
            if let Some(domain) = s.handle_domain()
                && covering_zone(domain, &s.dns.current_zones).is_some()
            {
                return ProviderStatus::ProviderHasDomain;
            }

            // RegisteredElsewhere: probe says the domain is registered
            // but it's NOT in this provider's zone list (we've already
            // ruled that out above).
            match domain_status {
                Some(DomainStatus::RegisteredWithNest) | Some(DomainStatus::RegisteredNoNest) => {
                    return ProviderStatus::RegisteredElsewhere;
                }
                _ => {}
            }

            // UnregisteredBuyable: domain unregistered, registrar quoted Buyable.
            if let Some(RegistrarAvailability::Buyable {
                price_cents,
                ref currency,
                ..
            }) = s.dns.current_availability
            {
                return ProviderStatus::UnregisteredBuyable {
                    price_cents,
                    currency: currency.clone(),
                };
            }

            // Otherwise: domain unregistered and not buyable (no registrar
            // cap, Unavailable, or TldNotSupported).
            ProviderStatus::UnregisteredNotBuyable
        })
    }

    pub fn can_continue_dns(&self) -> bool {
        use crate::state::ProviderStatus;
        use fauna_provisioning::dispatch;
        use fauna_provisioning::registrar::Registrar;

        let status = self.provider_status();
        match status {
            ProviderStatus::ProviderHasDomain => true,
            ProviderStatus::UnregisteredBuyable { .. } => self.with_state(|s| {
                if !s.dns.buy_domain || !s.dns.price_agreed {
                    return false;
                }
                // Only enforce contact when the registrar requires it.
                let requires_contact = s
                    .dns
                    .selected_provider_id
                    .as_deref()
                    .and_then(dispatch_provider_id)
                    .and_then(|pid| {
                        dispatch::registrar(
                            pid,
                            dispatch::Credentials::from_map(s.dns.creds.clone()),
                        )
                    })
                    .map(|r| r.requires_contact())
                    .unwrap_or(true);
                !requires_contact || s.dns.contact.is_some()
            }),
            ProviderStatus::RegisteredElsewhere
            | ProviderStatus::UnregisteredNotBuyable
            | ProviderStatus::NotReady => false,
        }
    }

    /// Sync transition. The actual buy / register happens in
    /// `start_provisioning()` so the user can still back out before
    /// committing money.
    pub fn continue_from_dns(&self) -> Result<(), OnboardingError> {
        self.mutate(|s| {
            // Mirror creds to VPS when same_provider_for_vps and the
            // provider supports VPS.
            if s.dns.same_provider_for_vps
                && let Some(pid) = s.dns.selected_provider_id.clone()
                && let Some(p) = PROVIDERS.iter().find(|p| p.id.as_str() == pid)
                && p.capabilities.contains(&Capability::Vps)
            {
                s.vps.selected_provider_id = Some(pid);
                s.vps.creds = s.dns.creds.clone();
                // A hosted sign-in done on the DNS form is done for the VPS
                // form too — same token, same provider.
                s.vps.hosted_auth = s.dns.hosted_auth.clone();
                s.vps.verified = true;
            }
            s.step = OnboardingStep::VpsConfig;
            Ok(())
        })
    }

    // ── VPS config stage ────────────────────────────────────────────────

    pub fn vps_config(&self) -> VpsConfigState {
        self.with_state(|s| s.vps.clone())
    }

    pub fn select_vps_provider(&self, id: String) {
        self.mutate(|s| {
            s.vps.selected_provider_id = Some(id);
            s.vps.creds.clear();
            s.vps.hosted_auth.clear();
            s.vps.verified = false;
            s.vps.server_types.clear();
            s.vps.selected_server_type_id = None;
            s.vps.locations.clear();
            s.vps.selected_location_id = None;
        });
    }

    pub fn select_vps_location(&self, id: String) {
        self.mutate(|s| s.vps.selected_location_id = Some(id));
    }

    pub fn set_vps_cred(&self, field_id: String, value: String) {
        self.mutate(|s| {
            s.vps.creds.insert(field_id, SecretString::from(value));
            s.vps.verified = false;
        });
    }

    pub fn select_vps_server_type(&self, id: String) {
        self.mutate(|s| s.vps.selected_server_type_id = Some(id));
    }

    /// Set the `vps-config-mail-mode-toggle`: whether the box provisions the mail
    /// subsystem. `true` = a mail box (clamd/rspamd scanner sidecars + mail ports
    /// `{25,465,587,993}`; needs a `mem_gb ≥ 2`
    /// plan); `false` = a social-only box (lean nest+watchtower compose, viable on
    /// the 1 GB tier — but it cannot later enable mail without a VPS resize). The
    /// decision must live here at `vps_config`, not the post-claim §3b
    /// enable-email checkbox, because cloud-init must know mail-intent (and the
    /// RAM, picked on this same page) *before* the box boots. Drives
    /// `CloudInitParams::enable_mail` via `run_provisioning_inner`. Defaults from
    /// [`Self::provision_mail_mode_enabled`] (the handle's real-domain default)
    /// until the user toggles it. Per `docs/goal/behavior/onboarding.md` §5.
    pub fn set_provision_mail_mode(&self, enabled: bool) {
        self.mutate(|s| {
            s.vps.enable_mail = Some(enabled);
            // Turning mail ON can make a previously-selected sub-2 GB plan
            // invalid; drop it so the box is never provisioned mail-on on a
            // plan the RAM gate forbids. Clients re-render the filtered radio
            // (no plan selected) and `can_continue_vps` blocks until the user
            // picks a mail-capable one. Single invariant every app relies
            // on, enforced once here via the shared gate helper.
            if let Some(sel) = s.vps.selected_server_type_id.clone() {
                let still_ok = s
                    .vps
                    .server_types
                    .iter()
                    .find(|st| st.id == sel)
                    .is_none_or(|st| {
                        crate::helpers::server_type_allowed_for_mail(st.clone(), enabled)
                    });
                if !still_ok {
                    s.vps.selected_server_type_id = None;
                }
            }
        });
    }

    /// Whether the `vps-config-mail-mode-toggle` is ON. Returns the user's
    /// explicit choice if set, else the handle's real-domain default
    /// ([`Self::handle_targets_real_domain`]) — the same predicate that seeds the
    /// §3b enable-email default, so a real-domain box defaults to a mail box and a
    /// `localhost` / IP target defaults to social-only. Clients read this to
    /// render the toggle's checked state and to gate the server-type radio (see
    /// [`server_type_allowed_for_mail`]). Per `docs/goal/behavior/onboarding.md`
    /// §5.
    pub fn provision_mail_mode_enabled(&self) -> bool {
        self.with_state(|s| s.vps.enable_mail)
            .unwrap_or_else(|| self.handle_targets_real_domain())
    }

    /// Select a `vps-config-update-channel-row`: which builds the box's
    /// automatic updater follows. Decided at `vps_config`, like the mail mode,
    /// because the channel's image tag is written into the box's cloud-init.
    /// Drives `CloudInitParams::image_tag` via `run_provisioning_inner`. Per
    /// `docs/goal/behavior/onboarding-provisioning.md` §5.
    pub fn set_provision_update_channel(
        &self,
        channel: fauna_provisioning::cloud_init::UpdateChannel,
    ) {
        self.mutate(|s| s.vps.update_channel = Some(channel));
    }

    /// The selected update channel — the user's explicit choice if set, else
    /// the default (`stable`). Clients read this to mark the selected
    /// `vps-config-update-channel-row`.
    pub fn provision_update_channel(&self) -> fauna_provisioning::cloud_init::UpdateChannel {
        self.with_state(|s| s.vps.update_channel)
            .unwrap_or_default()
    }

    /// Fields visible on the vps_config form. Filters the selected VPS
    /// provider's fields by `FieldMeta::kinds` containing `Capability::Vps`.
    /// Mirrors `visible_dns_fields()` so per-app view code is a one-liner
    /// (`_m.VisibleVpsFields()`) instead of a duplicated client-side filter.
    pub fn visible_vps_fields(&self) -> Vec<FieldMetaPlain> {
        self.with_state(|s| {
            let pid = match s.vps.selected_provider_id.as_deref() {
                Some(p) => p,
                None => return vec![],
            };
            let p = match PROVIDERS.iter().find(|p| p.id.as_str() == pid) {
                Some(p) => p,
                None => return vec![],
            };
            p.fields
                .iter()
                .filter(|f| f.kinds.contains(&Capability::Vps))
                .map(FieldMetaPlain::from)
                .collect()
        })
    }

    pub fn can_verify_vps(&self) -> bool {
        let visible = self.visible_vps_fields();
        self.with_state(|s| {
            !s.vps.verifying
                && visible.iter().all(|f| {
                    !f.required
                        || s.vps
                            .creds
                            .get(&f.id)
                            .map(|v| !v.is_empty())
                            .unwrap_or(false)
                })
        })
    }

    pub fn can_continue_vps(&self) -> bool {
        self.vps_continue_shortfall().is_none()
    }

    /// Why `vps-config-continue-button` is dead — `None` when it is live, else
    /// the one-line explainer naming **the act that revives it**.
    ///
    /// **This exists because a disabled control owes the user a reason**
    /// (`ui/README.md` § Copy comprehensibility rule 5; `apps/tui.md`
    /// § Rendering → *Control vocabulary* rule 3 restates it for the terminal,
    /// where DIM is the only other signal). The vps_config page shipped with
    /// **no** explanatory surface at all: a first-time user landed on four
    /// provider names, none marked, and a dead Continue, with nothing on screen
    /// saying what to do — the exact defect [`Self::dns_status_text_key`] was
    /// split in two to fix one page earlier in the same wizard, found here by
    /// the walk's wizard driver (`walk.rs`, 2026-08-05).
    ///
    /// Paired with [`Self::can_continue_vps`] over one private shortfall, so
    /// the verdict and its explanation cannot disagree — the
    /// `dns_provider_eligible` / `dns_provider_ineligible_reason` template. An
    /// `Option` rather than a "" key on purpose: a blank-string status is
    /// precisely the bug that left `dns-status-text` painting an empty line in
    /// its gating state, and `None` makes that unrepresentable.
    ///
    /// Four shortfalls, four messages, not one generic line (rule 5 Q2 — the
    /// user's next act differs in each: pick, verify, choose where, choose how
    /// big).
    pub fn vps_continue_blocked_reason(&self) -> Option<LocalizedText> {
        Some(LocalizedText::key(match self.vps_continue_shortfall()? {
            VpsShortfall::NoProvider => "onboarding.vps_config.status_pick_provider",
            VpsShortfall::Unverified => "onboarding.vps_config.status_verify_credentials",
            VpsShortfall::NoLocation => "onboarding.vps_config.status_pick_location",
            VpsShortfall::NoServerType => "onboarding.vps_config.status_pick_server_type",
        }))
    }

    // Provisioning step (page 6) button affordances. These read the live
    // `provisioning_snapshot().overall` and delegate to the typed predicates
    // on `OverallStatus`, so the rules in `onboarding.md` §6 live in exactly
    // one place across all apps — mirroring `can_continue_vps`/
    // `can_continue_dns` for the sibling stages. Native apps may also call
    // the inherent `OverallStatus` predicates directly on the snapshot they
    // already hold; web reaches them through these methods via wasm.

    /// Cancel button is shown — provisioning is actively running. Also gates
    /// the elapsed-time ticker.
    pub fn provisioning_in_progress(&self) -> bool {
        self.provisioning_snapshot().overall.is_running()
    }

    /// Retry button is shown — the run Failed or was soft-Cancelled.
    pub fn can_retry_provisioning(&self) -> bool {
        self.provisioning_snapshot().overall.can_retry()
    }

    /// Wizard-exit Continue button enables — provisioning Succeeded.
    pub fn can_continue_provisioning(&self) -> bool {
        self.provisioning_snapshot().overall.can_continue()
    }

    /// Why the wizard-exit Continue button is dead — `ui/README.md` rule 5:
    /// the four `○` step glyphs are a symbol, not a reason. One message per
    /// blocked `OverallStatus` (idle/running/failed/cancelled), because the
    /// user's next act differs: start, wait, retry.
    pub fn provisioning_continue_blocked_reason(&self) -> Option<LocalizedText> {
        self.provisioning_snapshot()
            .overall
            .continue_blocked_reason()
    }

    pub async fn verify_vps(&self) -> Result<(), OnboardingError> {
        use fauna_provisioning::dispatch;
        use fauna_provisioning::vps::VpsProvider;

        // See `verify_dns` for the cross-app rationale: gating
        // `can_verify_vps()` on the verifying flag keeps every app's
        // verify-button disabled while the probe is in flight.
        self.mutate(|s| s.vps.verifying = true);
        let clear_verifying = |m: &Self| m.mutate(|s| s.vps.verifying = false);

        let snap = self.snapshot();
        let pid = match snap.vps.selected_provider_id.clone() {
            Some(p) => p,
            None => {
                clear_verifying(self);
                return Err(OnboardingError::InvalidTransition {
                    from: "vps_config".into(),
                    reason: "no VPS provider selected".into(),
                });
            }
        };
        let creds = snap.vps.creds.clone();

        let provider_id = match dispatch_provider_id(&pid) {
            Some(p) => p,
            None => {
                clear_verifying(self);
                return Err(OnboardingError::Other {
                    detail: format!("unknown provider id: {pid}"),
                });
            }
        };

        let vps_dispatch = match dispatch::vps_provider(
            provider_id,
            dispatch::Credentials::from_map(creds.clone()),
            // Honor the `provider_base_urls["vps"]` E2E override so the
            // verify + server-type-list probes hit the fake instead of
            // the live cloud API (mirrors `run_provisioning_inner`).
            // `None` in production.
            self.provider_base_url("vps".into()),
        ) {
            Some(d) => d,
            None => {
                clear_verifying(self);
                return Err(OnboardingError::Other {
                    detail: format!("provider {pid} doesn't support VPS"),
                });
            }
        };

        // Verify credentials and capture available locations for the picker.
        let locations = match vps_dispatch.verify(&self.http).await {
            Ok(locs) => locs,
            Err(e) => {
                let msg = e.to_string();
                self.mutate(|s| {
                    s.set_error(format!("Verify failed: {msg}"));
                    s.vps.verifying = false;
                });
                return Err(OnboardingError::ProviderUnauthorized { provider: pid });
            }
        };

        // Pull curated server types from the provider registry.
        let curated: Vec<&str> = PROVIDERS
            .iter()
            .find(|p| p.id.as_str() == pid)
            .map(|p| p.curated_offers)
            .unwrap_or(&[])
            .to_vec();

        let types = match vps_dispatch.list_server_types(&self.http, &curated).await {
            Ok(types) => types,
            Err(e) => {
                let msg = e.to_string();
                self.mutate(|s| {
                    s.set_error(format!("Server type list failed: {msg}"));
                    s.vps.verifying = false;
                });
                return Err(OnboardingError::ProviderProtocolError {
                    provider: pid,
                    detail: msg,
                });
            }
        };

        // Default location is the first one returned. Clients can override
        // via `select_vps_location()` to surface a real picker.
        let default_location = locations.first().map(|l| l.id.clone());

        self.mutate(|s| {
            s.vps.verified = true;
            s.vps.server_types = types.into_iter().take(5).collect();
            s.vps.locations = locations;
            if s.vps.selected_location_id.is_none() {
                s.vps.selected_location_id = default_location;
            }
            s.error_message = None;
            s.vps.verifying = false;
        });
        Ok(())
    }

    /// VPS-stage Continue button (`vps-config-continue-button`). Validates
    /// the form (verified credentials, location chosen, server type
    /// chosen) and transitions the wizard to `NestProvisioning`. The
    /// orchestrator is kicked separately by the architecture-defined
    /// "Buy and set up" CTA on the `nest_provisioning` page, which calls
    /// `start_provisioning()` — that gives the user a final price-review
    /// gate before money is committed.
    pub async fn continue_from_vps(&self) -> Result<(), OnboardingError> {
        if !self.can_continue_vps() {
            return Err(OnboardingError::InvalidTransition {
                from: "vps_config".into(),
                reason: "VPS not ready: verify credentials, pick a location and server type first"
                    .into(),
            });
        }
        self.mutate(|s| {
            s.step = OnboardingStep::NestProvisioning;
            s.error_message = None;
        });
        Ok(())
    }

    /// Returns the DNS records the deferred-DNS orchestrator captured.
    /// Empty until `start_provisioning` runs the deferred path successfully.
    /// Read by clients on the `dns_post_instructions` page.
    pub fn dns_records(&self) -> Vec<DnsRecordPlain> {
        self.with_state(|s| s.dns_records.clone())
    }

    /// Renders the captured DNS records as the markdown table the
    /// `dns_post_instructions` page surfaces — same shape the orchestrator's
    /// `DeferredDnsResult.instructions_markdown` produces. Returns `None`
    /// when records or the provisioning result aren't populated yet
    /// (i.e. the deferred-DNS run hasn't finished). Clients that prefer to
    /// render their own table can read `dns_records()` and the snapshot's
    /// result directly.
    pub fn dns_post_instructions(&self) -> Option<String> {
        let records = self.dns_records();
        if records.is_empty() {
            return None;
        }
        let result = self.provisioning_snapshot().result?;
        let domain = result.domain;
        let ipv4 = result.ipv4;
        let mut s = format!(
            "# DNS records for {domain}\n\n\
             Add these records at your DNS host. The server is running at \
             `{ipv4}` — once the records propagate (usually 1–10 minutes) \
             your nest will come online at `https://{domain}`. Then return \
             to the app and continue.\n\n\
             | Type | Name | Value | TTL | Priority |\n\
             |------|------|-------|-----|----------|\n"
        );
        for r in records {
            let prio = r
                .priority
                .map(|p| p.to_string())
                .unwrap_or_else(|| "—".into());
            s.push_str(&format!(
                "| {} | `{}` | `{}` | {} | {} |\n",
                r.record_type, r.name, r.value, r.ttl, prio
            ));
        }
        Some(s)
    }

    // ── Provisioning stage ──────────────────────────────────────────────

    /// The `nest_provisioning` page's top-region price summary — up to two
    /// line items: the domain's one-time registration price (only when the
    /// wizard is buying a new domain) and the selected VPS's monthly price
    /// (always present — `vps_config`'s Continue requires a selection).
    /// Both prices were already shown and, for the domain, explicitly
    /// agreed to earlier in the wizard (`dns-tld-price-display` /
    /// `dns-price-confirm-checkbox` on `dns_config`, the server-type radio
    /// options on `vps_config`); this is a pre-commit recap before
    /// `provisioning-start-button`, not a new price source. Pure
    /// computation over already-in-state DNS/VPS data — no IO.
    /// `docs/goal/behavior/onboarding.md` §6.
    pub fn bill_of_materials(&self) -> Vec<crate::state::BillOfMaterialsItem> {
        use crate::state::{BillOfMaterialsItem, ProviderStatus};
        use fauna_provisioning::progress::{ProvisionStep, step_label};

        let mut items = Vec::with_capacity(2);

        // `provider_status()` locks state itself, so compute it before
        // (not inside) the `with_state` call below — it's a separate,
        // sequential lock acquisition, not a nested one.
        let buy_domain_price = if self.with_state(|s| s.dns.buy_domain)
            && let ProviderStatus::UnregisteredBuyable {
                price_cents,
                currency,
            } = self.provider_status()
        {
            Some((price_cents, currency))
        } else {
            None
        };
        self.with_state(|s| {
            if let Some((price_cents, currency)) = buy_domain_price {
                // The renewal price rides the same `Buyable` quote the
                // first-year price came from (`onboarding.md` § 6: disclosed
                // before the charge; `None` when the registrar quoted none).
                let renewal_price_cents = match s.dns.current_availability {
                    Some(fauna_provisioning::registrar::RegistrarAvailability::Buyable {
                        renewal_cents,
                        ..
                    }) => renewal_cents,
                    _ => None,
                };
                items.push(BillOfMaterialsItem {
                    label: step_label(ProvisionStep::Domain),
                    price_cents,
                    // Same USD fallback `dns_status_text`'s `UnregisteredBuyable`
                    // arm uses for a registrar quote with no currency code.
                    currency: currency.unwrap_or_else(|| "USD".into()),
                    recurring: false,
                    renewal_price_cents,
                });
            }

            if let Some(server_type) = s
                .vps
                .selected_server_type_id
                .as_deref()
                .and_then(|id| s.vps.server_types.iter().find(|t| t.id == id))
            {
                items.push(BillOfMaterialsItem {
                    label: step_label(ProvisionStep::Server),
                    price_cents: server_type.price_monthly_cents,
                    currency: server_type.currency.clone(),
                    recurring: true,
                    renewal_price_cents: None,
                });
            }
        });

        items
    }

    /// `bill_of_materials()`'s non-recurring (domain) item, pre-folded into
    /// one [`LocalizedText`] ready for `resolve_nested` — `None` when
    /// nothing is chargeable. `{label}` carries the step's own i18n key
    /// (never its own args — `step_label` is always a bare key — so passing
    /// it as a `resolve_nested` arg is safe) and `{price}`/`{renewal}` the
    /// shared [`crate::helpers::format_price`] output. Picks the
    /// `bom_line_domain` key over plain `bom_line` when the registrar
    /// quoted a renewal price (`onboarding.md` § 6: disclosed before the
    /// charge). Lifted out of per-app derivation — linux and tui carried
    /// mirrored, independently-drifting copies of this exact branch.
    pub fn bom_domain_line(&self) -> Option<LocalizedText> {
        let item = self
            .bill_of_materials()
            .into_iter()
            .find(|i| !i.recurring)?;
        let price = crate::helpers::format_price(item.price_cents, item.currency.clone());
        Some(match item.renewal_price_cents {
            Some(renewal_cents) => {
                let renewal = crate::helpers::format_price(renewal_cents, item.currency);
                LocalizedText::key_args(
                    "onboarding.nest_provisioning.bom_line_domain",
                    [
                        ("label", item.label.key),
                        ("price", price),
                        ("renewal", renewal),
                    ],
                )
            }
            None => LocalizedText::key_args(
                "onboarding.nest_provisioning.bom_line",
                [("label", item.label.key), ("price", price)],
            ),
        })
    }

    /// `bill_of_materials()`'s recurring (VPS) item, pre-folded into one
    /// [`LocalizedText`] ready for `resolve_nested` — `None` when nothing is
    /// selected yet. Always the `bom_line_recurring` key: VPS pricing has no
    /// first-year/renewal split. See [`Self::bom_domain_line`] for the
    /// `{label}`-is-a-key rationale.
    pub fn bom_vps_line(&self) -> Option<LocalizedText> {
        let item = self.bill_of_materials().into_iter().find(|i| i.recurring)?;
        let price = crate::helpers::format_price(item.price_cents, item.currency);
        Some(LocalizedText::key_args(
            "onboarding.nest_provisioning.bom_line_recurring",
            [("label", item.label.key), ("price", price)],
        ))
    }

    /// Returns a clone of the current provisioning snapshot. Cheap (clones
    /// a small struct). Pull-based — clients re-read on every observer
    /// tick rather than receiving the snapshot via callback.
    ///
    /// The clone is `enrich_display`-ed so every app reads the canonical
    /// per-step visibility booleans (`shows_substep`/`shows_error`/
    /// `shows_attempt_suffix`) instead of re-deriving the rule. Computed here on
    /// the outgoing clone — never on the live mutated state, which the
    /// `set_cancelled` path mutates outside `with_step` (see `recompute_display`).
    pub fn provisioning_snapshot(&self) -> ProvisioningSnapshot {
        let mut snap = self.provisioning.lock().unwrap().clone();
        snap.enrich_display();
        snap
    }

    /// Sets the cancel flag. The running provisioning task observes it at
    /// the next step boundary or retry iteration. Soft-cancel only —
    /// already-created VPS/DNS resources stay; a subsequent retry picks
    /// them up via idempotency.
    pub fn cancel_provisioning(&self) {
        self.provisioning_cancel.raise();
        self.observer.on_changed();
    }

    /// Re-runs provisioning from the top. Idempotency makes already-done
    /// steps short-circuit on re-run, so this is functionally equivalent
    /// to "resume from the failed step." Same fire-and-forget shape as
    /// `start_provisioning`.
    ///
    /// Resuming means the SAME box: the re-run reuses the claim code and
    /// expects the identity the box was built with (`pending_provision_row`),
    /// rather than minting fresh ones a box that already exists can never
    /// match. Only the run's own progress snapshot is reset here.
    pub fn retry_provisioning(self: Arc<Self>) {
        self.provisioning_cancel.reset();
        *self.provisioning.lock().unwrap() = ProvisioningSnapshot::idle();
        self.observer.on_changed();
        self.start_provisioning();
    }

    /// Spawns the four-step provisioning orchestrator on the appropriate
    /// runtime (`tokio::spawn` on native, `wasm_bindgen_futures::spawn_local`
    /// on web) and returns immediately. All inputs are read from the
    /// machine's existing state (handle, DNS provider/creds, VPS
    /// provider/creds, contact, set-up-later flag). Observer ticks drive
    /// re-render; the client reads `provisioning_snapshot()` on each tick.
    ///
    /// Idempotent: safe to call after a partial prior run — the
    /// orchestrator's pre-flight checks short-circuit completed work.
    pub fn start_provisioning(self: Arc<Self>) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            tokio::spawn(self.run_provisioning());
        }
        #[cfg(target_arch = "wasm32")]
        {
            wasm_bindgen_futures::spawn_local(self.run_provisioning());
        }
    }

    /// Runs the four-step orchestrator **to completion** (the future
    /// `start_provisioning` spawns) and stashes the terminal result. Observer
    /// ticks drive re-render throughout; `provisioning_snapshot()` / the
    /// `provisioning_cancel` flag stay readable/settable from another thread.
    ///
    /// `start_provisioning` is the fire-and-forget spawn wrapper used by web
    /// (`spawn_local`) and any native caller already inside a tokio runtime.
    /// Native apps that have **no** runtime on their UI thread during
    /// onboarding (the GTK main thread on linux — `tokio::spawn` there would
    /// panic) instead `await` this on a worker runtime
    /// (`async_helper::run_on_tokio`). Idempotent like `start_provisioning`:
    /// `run_provisioning_inner` resets the snapshot + cancel flag at entry, so
    /// it doubles as the retry/resume entry point.
    pub async fn run_provisioning(self: Arc<Self>) {
        let result = run_provisioning_inner(self.clone()).await;
        stash_provisioning_result(self, result);
    }

    /// Test-aware nest base URL: the override map wins
    /// when set; otherwise the wizard's state.nest_url is used.
    ///
    /// Use only for HTTP request URLs. For outcome values that identify
    /// the nest to per-app glue (e.g. `WizardOutcome::AwaitingManualDns`),
    /// read `state.nest_url` directly — the override must not leak into
    /// persisted outcome data.
    fn effective_nest_url(&self) -> String {
        self.provider_base_url("nest".into())
            .unwrap_or_else(|| self.state.lock().unwrap().nest_url.clone())
    }

    /// i18n-aware variant of the DNS status text. Returns the i18n key plus
    /// the substitution map. Clients pass `(key, args)` through their
    /// platform's localization pipeline (Apple `Bundle.main.localizedString`,
    /// Android `getString`, Web `L()`, etc.). The string keys live in
    /// `i18n/strings/en.yaml` under `onboarding.dns_config.status_*`.
    pub fn dns_status_text_key(&self) -> LocalizedText {
        use crate::state::ProviderStatus;
        let provider = self
            .with_state(|s| s.dns.selected_provider_id.clone())
            .unwrap_or_default();
        match self.provider_status() {
            // `NotReady` is the entry state, where Continue is disabled — so a
            // blank line here left a dead control unexplained on all 7 apps at
            // once (`ui/README.md` § Copy comprehensibility rule 5). It covers
            // two conditions, and rule 5's Q2 wants two messages, not one
            // generic one: the user's next act is different in each.
            ProviderStatus::NotReady => LocalizedText::key(
                if self.with_state(|s| s.dns.selected_provider_id.is_none()) {
                    "onboarding.dns_config.status_pick_provider"
                } else {
                    "onboarding.dns_config.status_verify_credentials"
                },
            ),
            ProviderStatus::ProviderHasDomain => {
                let mut args = std::collections::HashMap::new();
                args.insert("provider".into(), provider);
                LocalizedText {
                    key: "onboarding.dns_config.status_owned".into(),
                    args,
                }
            }
            ProviderStatus::RegisteredElsewhere => {
                let mut args = std::collections::HashMap::new();
                args.insert("provider".into(), provider);
                LocalizedText {
                    key: "onboarding.dns_config.status_registered_elsewhere".into(),
                    args,
                }
            }
            ProviderStatus::UnregisteredBuyable {
                price_cents,
                currency,
            } => {
                let price = crate::helpers::format_price(
                    price_cents,
                    currency.unwrap_or_else(|| "USD".into()),
                );
                let mut args = std::collections::HashMap::new();
                args.insert("provider".into(), provider);
                args.insert("price".into(), price);
                LocalizedText {
                    key: "onboarding.dns_config.status_buyable".into(),
                    args,
                }
            }
            ProviderStatus::UnregisteredNotBuyable => {
                let mut args = std::collections::HashMap::new();
                args.insert("provider".into(), provider);
                LocalizedText {
                    key: "onboarding.dns_config.status_not_buyable".into(),
                    args,
                }
            }
        }
    }

    /// Stage-aware Back button. The wizard's nav graph is small enough to
    /// hardcode here so each app doesn't replicate the routing.
    pub fn back(&self) {
        self.mutate(|s| {
            s.error_message = None;
            s.step = match s.step {
                OnboardingStep::IdentityCreated | OnboardingStep::IdentityImport => {
                    OnboardingStep::IdentityChoice
                }
                OnboardingStep::HandleEntry => match s.identity_origin {
                    Some(IdentityOrigin::Created) => OnboardingStep::IdentityCreated,
                    Some(IdentityOrigin::Imported) => OnboardingStep::IdentityImport,
                    None => OnboardingStep::IdentityChoice,
                },
                OnboardingStep::DnsConfig => OnboardingStep::HandleEntry,
                // Recovery cloud path enters VpsConfig straight from NestRecovery
                // (dns_config is skipped — the recovered box's domain + DNS creds
                // come from the custody), so back returns to the box-selection hub.
                OnboardingStep::VpsConfig if s.recovery_intent => OnboardingStep::NestRecovery,
                OnboardingStep::VpsConfig => OnboardingStep::DnsConfig,
                OnboardingStep::DnsPostInstructions => OnboardingStep::VpsConfig,
                OnboardingStep::InviteRequest => OnboardingStep::HandleEntry,
                OnboardingStep::ClaimCode => OnboardingStep::HandleEntry,
                OnboardingStep::NestProvisioning => OnboardingStep::VpsConfig,
                // `recover-back-button` on nest_recovery (box-recovery.md §
                // Recovery UI): came-from-identity returns to handle_entry (Q2-A
                // routes recovery through the nest-connect step, so that's the
                // immediate predecessor now, not identity_import); came-from-launch
                // is a wizard exit (launch_retry is a launch-flow surface, not an
                // OnboardingStep) — stay put and let the launch glue tear the
                // wizard down and re-show launch_retry.
                OnboardingStep::NestRecovery => match s.recovery_came_from {
                    Some(BoxRecoveryEntry::Identity) => OnboardingStep::HandleEntry,
                    Some(BoxRecoveryEntry::Launch) | None => s.step,
                },
                OnboardingStep::RecoverSelfhostedInstructions => OnboardingStep::NestRecovery,
                // recovery-entry-back-button → identity_choice (ui.yaml
                // `recovery_entry`).
                OnboardingStep::RecoveryEntry => OnboardingStep::IdentityChoice,
                // Terminal states — back stays put. (`NatModeChoice` has no
                // Back by design: the admin is already server-committed —
                // onboarding.md § 3b-bis / ui.yaml `nat_mode_choice`.
                // `RecoveryKit` has no Back either: confirm and skip are its
                // only exits — ui.yaml `recovery_kit`. `TrustPrompt` likewise:
                // grant and skip are its only exits, and every predecessor it
                // has is already server-committed — the NAT page on the claim
                // path, a completed registration on either join route
                // — ui.yaml `trust_prompt`.)
                OnboardingStep::IdentityChoice
                | OnboardingStep::Done
                | OnboardingStep::NatModeChoice
                | OnboardingStep::TrustPrompt
                | OnboardingStep::RecoveryKit => s.step,
            };
            // Backing out of the recovery import all the way to IdentityChoice
            // exits the recovery branch — clear the intent so a subsequent normal
            // create/import isn't mis-routed to NestRecovery.
            if s.step == OnboardingStep::IdentityChoice {
                Self::clear_recovery(s);
            }
        });
    }

    // ── Total-box-loss recovery branch (box-recovery.md § Recovery UI, step 4) ──

    /// `recover-lost-box-button` on `identity_choice` (the fresh re-onboarded
    /// client entry). Marks recovery intent and routes to `identity_import`
    /// first — the admin's identity must be loaded to read the
    /// `fauna.state.deployment-seeds` map) — then `confirm_imported_identity` lands on
    /// `NestRecovery` (the `condition: "recovery-intent"` transition).
    pub fn begin_recover_lost_box(&self) {
        self.mutate(|s| {
            s.recovery_intent = true;
            s.recovery_came_from = Some(BoxRecoveryEntry::Identity);
            s.recovery_selected_nest_id = None;
            s.identity_origin = Some(IdentityOrigin::Imported);
            s.step = OnboardingStep::IdentityImport;
            s.error_message = None;
        });
    }

    /// `launch-recover-button` on `launch_retry` (the surviving-device entry).
    /// The client already holds its identity, so the glue passes it here; this
    /// seeds it and drops straight into `NestRecovery` (the synced
    /// `fauna.state.deployment-seeds` map is guaranteed local). Mirrors `seed_identity`, but for the
    /// recovery branch. No validation — the caller owns a valid 64-hex secret.
    pub fn seed_identity_for_recovery(&self, secret: String) {
        self.mutate(|s| {
            s.imported_secret = Some(secret.into());
            // Left `None` (not `Imported`) — the seeded user never visited
            // IdentityImport this session (mirrors `seed_identity`).
            s.identity_origin = None;
            s.recovery_intent = true;
            s.recovery_came_from = Some(BoxRecoveryEntry::Launch);
            s.recovery_selected_nest_id = None;
            s.step = OnboardingStep::NestRecovery;
            s.error_message = None;
        });
    }

    /// Push the custodied box list into the machine for `nest_recovery` to
    /// render (`recover-box-item`). The per-app glue fetches it from a
    /// reachable nest via the shared `deploymentSeeds()` getter (owner-secret
    /// read of `fauna.state.deployment-seeds`) and hands the resulting `nest_actor_id` (hex) list
    /// here — held on the machine so every app renders one uniform list
    /// (like `verify_vps` populating `vps.server_types`). Only public
    /// `nest_actor_id`s cross; the seed stays in custody. A selection that is
    /// no longer in the new list is cleared.
    pub fn set_recovery_boxes(&self, boxes: Vec<String>) {
        self.mutate(|s| {
            if let Some(sel) = &s.recovery_selected_nest_id
                && !boxes.contains(sel)
            {
                s.recovery_selected_nest_id = None;
            }
            s.recovery_boxes = boxes;
        });
    }

    /// The custodied box list rendered on `nest_recovery` — one `nest_actor_id`
    /// (hex) per box, empty when nothing is custodied / not yet synced
    /// (`recover-box-empty-message`).
    pub fn recovery_boxes(&self) -> Vec<String> {
        self.with_state(|s| s.recovery_boxes.clone())
    }

    /// Select a custodied box on `nest_recovery` (`recover-box-item`). Records
    /// the box's `nest_actor_id` (hex); the method buttons stay disabled until a
    /// box is selected. The raw seed is resolved in Rust at re-provision time
    /// (`deployment_seed_for`), never surfaced here.
    pub fn select_recovery_box(&self, nest_actor_id: String) {
        self.mutate(|s| {
            s.recovery_selected_nest_id = Some(nest_actor_id);
            s.error_message = None;
        });
    }

    /// `recover-method-cloud-button` on `nest_recovery`: re-provision the
    /// selected box via a cloud VPS. Advances to `vps_config` in recovery mode
    /// (the shared orchestrator re-provisions with the saved seed installed and
    /// re-points A/AAAA as its `Dns` step — the drive is a later slice). Errors
    /// if no box is selected.
    pub fn recover_via_cloud(&self) -> Result<(), OnboardingError> {
        self.require_selected_recovery_box()?;
        self.mutate(|s| {
            s.step = OnboardingStep::VpsConfig;
            s.error_message = None;
        });
        Ok(())
    }

    /// `recover-method-selfhosted-button` on `nest_recovery`: re-provision the
    /// selected box via a self-hosted installer. Advances to
    /// `recover_selfhosted_instructions` (the installer command carrying
    /// `FAUNA_DEPLOYMENT_SEED`). Errors if no box is selected.
    pub fn recover_via_selfhosted(&self) -> Result<(), OnboardingError> {
        self.require_selected_recovery_box()?;
        self.mutate(|s| {
            s.step = OnboardingStep::RecoverSelfhostedInstructions;
            s.error_message = None;
        });
        Ok(())
    }

    /// Whether the wizard is in the total-box-loss recovery branch. The web glue
    /// gates the entry CTAs / recovery-mode provisioning on this.
    pub fn recovery_intent(&self) -> bool {
        self.with_state(|s| s.recovery_intent)
    }

    /// Which entry the recovery branch was reached from, or `None` outside it.
    /// The glue uses this to route `recover-back-button` (came-from-launch tears
    /// the wizard down to `launch_retry`; came-from-identity is `back()`).
    pub fn recovery_came_from(&self) -> Option<BoxRecoveryEntry> {
        self.with_state(|s| s.recovery_came_from)
    }

    /// The `nest_actor_id` (hex) of the selected box on `nest_recovery`, or
    /// `None` before a selection. Drives the `recover-box-item` selected state.
    pub fn recovery_selected_nest_id(&self) -> Option<String> {
        self.with_state(|s| s.recovery_selected_nest_id.clone())
    }
}

impl OnboardingMachine {
    /// Guard for the recovery method buttons: a box must be selected first.
    fn require_selected_recovery_box(&self) -> Result<(), OnboardingError> {
        if self.with_state(|s| s.recovery_selected_nest_id.is_some()) {
            Ok(())
        } else {
            self.mutate(|s| {
                s.set_error("Select a box to recover first".into());
            });
            Err(OnboardingError::InvalidTransition {
                from: "nest_recovery".into(),
                reason: "no recovery box selected".into(),
            })
        }
    }
}

// ── Handle-check snapshot getters ──────────────────────────────────────────

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl OnboardingMachine {
    pub fn handle_check_snapshot(&self) -> HandleCheckSnapshot {
        self.handle_check.lock().unwrap().clone()
    }

    pub fn invite_request_snapshot(&self) -> InviteRequestSnapshot {
        let mut snap = self.invite_request.lock().unwrap().clone();
        // Per target-doc Architectural rule 4, the OOB-row status label is
        // derived from `out_of_band_code_state` in shared Rust, not on the
        // client. Refresh on every read so transitions that forget to set
        // it can't leak stale text.
        snap.oob_message =
            crate::snapshots::invite_request::oob_message_for(&snap.out_of_band_code_state);
        // The admin-flow row's label is derived the same way, and for the same
        // reason — except here "transitions that forget to set it" was not a
        // hypothetical: EVERY transition forgot. `message` was written once by
        // `InviteRequestSnapshot::idle()` and never again, so this row showed
        // the idle copy through Submitting / PendingReview / Denied / errors on
        // all 7 apps. See `message_for` for the full account. It is total as of
        // 2026-08-12 (the one `None` arm retired with the dead `Approved`
        // variant), so this is an unconditional write — there is no longer a
        // state that keeps stale stored text.
        snap.message = crate::snapshots::invite_request::message_for(&snap.state);
        // The onboarding notice is derived the same way, from the claim the
        // platform glue handed `set_age_claim` — the shells only render
        // (`family-safety.md` § App surface → *Age-band surfaces*).
        // It follows the claim that will be SENT, so it says "verified" only
        // for an attestation the guard lets onto the wire.
        snap.age_notice = self.age_claim_to_send().and_then(|claim| {
            fauna_protocol::age::age_notice(
                &claim.band,
                claim.attestation.as_ref().map(|a| a.platform.as_str()),
            )
        });
        snap
    }

    pub fn wizard_outcome(&self) -> Option<WizardOutcome> {
        self.outcome.lock().unwrap().clone()
    }

    /// The DNS-provider credential captured at the onboarding DNS step, in the
    /// shape the launched client's `DnsManagementMachine::PutCredentials`
    /// consumes. `None` unless a provider was selected, `verify_dns()`
    /// succeeded, and the admin did **not** choose "set up later" — i.e. there
    /// is a verified credential worth sealing.
    ///
    /// This is the onboarding→launch **hand-off channel** for the client-side
    /// DNS credential store (`docs/goal/behavior/dns-management.md` § Where the
    /// credential lives; `docs/goal/behavior/onboarding.md` § 4). The
    /// onboarding machine has no account-plane write capability and, in the
    /// fresh-provision path, no live nest "at the end of the DNS step"; so
    /// rather than sealing here it exposes the captured credential and the
    /// **launched client** seals it once authenticated, via the normal
    /// `PutCredentials` path — one store, one writer (the
    /// `DnsManagementMachine`), no second config-put path in onboarding. The
    /// per-app launch glue reads this at `wizard_outcome() == LoggedIn` and
    /// dispatches `PutCredentials { provider_id, fields, label }` through
    /// `build_dns_management_machine_with_credentials`. The machine re-runs
    /// `verify()` to (re)derive covered zones, so the captured zones are not
    /// re-surfaced here.
    pub fn captured_dns_credential(&self) -> Option<CapturedDnsCredential> {
        self.with_state(|s| {
            let provider_id = s.dns.selected_provider_id.clone()?;
            if !s.dns.verified || s.dns.set_up_later {
                return None;
            }
            let label = match s.handle_domain() {
                Some(domain) => format!("{provider_id} ({domain})"),
                None => provider_id.clone(),
            };
            Some(CapturedDnsCredential {
                provider_id,
                fields: s.dns.creds.clone(),
                label,
            })
        })
    }

    /// Re-root the re-provision drive's custody read at a sandboxed phone
    /// shell's account-store container — the same container the shell hands the
    /// account runtime (`fauna_client_account_runtime::SandboxedStoreContainer`),
    /// so the local read opens the store the runtime writes. `None` restores the
    /// per-OS platform root, which every desktop host and web use without ever
    /// calling this.
    pub fn set_store_container_dir(&self, store_container_dir: Option<String>) {
        use fauna_account_plane::deployment_seed_recovery::StoreRoot;
        let root = match store_container_dir {
            Some(dir) => StoreRoot::at(dir),
            None => StoreRoot::platform(),
        };
        *self.recovery_config_reader.write().unwrap() =
            crate::recovery_config::production_reader(root);
    }

    /// Snapshot for the `claim_code` page. Pure read; cheap clone of a
    /// small enum + `LocalizedText`. Per
    /// `docs/goal/behavior/onboarding.md` §3a — clients render this on every
    /// observer tick.
    pub fn claim_code_snapshot(&self) -> ClaimCodeSnapshot {
        self.claim_code.lock().unwrap().clone()
    }

    /// Snapshot for the `nat_mode_choice` page. Pure read.
    /// Per `docs/goal/behavior/onboarding.md` § 3b-bis.
    pub fn nat_mode_snapshot(&self) -> NatModeSnapshot {
        self.nat_mode.lock().unwrap().clone()
    }

    /// Serializes the current `InviteRequestState` for the per-app
    /// pending-invite slot. The format is opaque to clients — the wizard
    /// parses it back via `seed_pending_invite`. Returns `""` on the
    /// unreachable case where serde fails (the enum derives Serialize).
    pub fn pending_invite_status_json(&self) -> String {
        let state = self.invite_request.lock().unwrap().state.clone();
        serde_json::to_string(&state).unwrap_or_default()
    }

    /// Place the wizard at `InviteRequest` with handle and nest_url
    /// hydrated, but no pending-invite record. Used by the per-app
    /// app-launch glue when the silent challenge reports the secret
    /// isn't registered on a known-good nest — the user is still on
    /// the right nest, just needs an invite. Equivalent in shape to
    /// `seed_pending_invite` but without a `request_id` or status JSON.
    /// Per the onboarding client target-state design's app-launch
    /// failure-mode table (tracked internally).
    pub fn seed_at_invite_request_unregistered(&self, handle: String, nest_url: String) {
        self.mutate(|s| {
            s.handle = handle;
            s.nest_url = nest_url;
            s.step = OnboardingStep::InviteRequest;
        });
        *self.invite_request.lock().unwrap() = InviteRequestSnapshot::idle();
        self.observer.on_changed();
    }

    /// Whether the currently-selected DNS registrar requires per-
    /// registration WHOIS contact info (Gandi today; Porkbun = false).
    /// Drives the `dns-contact-form` visibility per the target-state
    /// doc's §4. Returns `false` when no provider is selected, when
    /// the provider has no `registrar` capability, or when no creds
    /// have been entered yet — the form only appears once a registrar
    /// has been chosen and (post-`verify_dns`) the wizard has
    /// confirmed the provider's contact requirements.
    pub fn selected_registrar_requires_contact(&self) -> bool {
        use fauna_provisioning::dispatch;
        use fauna_provisioning::registrar::Registrar;
        self.with_state(|s| {
            s.dns
                .selected_provider_id
                .as_deref()
                .and_then(dispatch_provider_id)
                .and_then(|pid| {
                    dispatch::registrar(pid, dispatch::Credentials::from_map(s.dns.creds.clone()))
                })
                .map(|r| r.requires_contact())
                .unwrap_or(false)
        })
    }

    /// Whether the DNS-provider button for `provider_id` should be selectable
    /// given the user's current DNS choices. A provider is *ineligible* when
    /// the user wants to buy a domain but it can't register
    /// (`buy_domain && !Registrar`), or wants one provider for both DNS and
    /// VPS but it has no VPS capability (`same_provider_for_vps && !Vps`).
    /// Unknown ids are never eligible. This is the queryable form of the
    /// deselect-on-toggle guards in `toggle_buy_domain` /
    /// `toggle_same_provider_for_vps`; all apps drive per-provider button
    /// sensitivity through it instead of re-deriving the capability rule.
    pub fn dns_provider_eligible(&self, provider_id: String) -> bool {
        matches!(
            self.dns_provider_shortfalls(&provider_id),
            Some((false, false))
        )
    }

    /// Why the `dns-provider-row[<id>]` control is not selectable — `None` when
    /// it is, else the i18n key of a one-line explainer naming the constraint
    /// that closed it *and the checkbox that re-opens it*.
    ///
    /// **This exists because a disabled control owes the user a reason**
    /// (`ui/README.md` § Copy comprehensibility rule 5 — the cross-app owner;
    /// `apps/tui.md` § Rendering → *Control vocabulary* rule 3 restates it for
    /// the terminal, where DIM is the only other signal). [`Self::
    /// dns_provider_eligible`] answers yes/no, which is enough to grey a row
    /// out and not enough to explain it; a shell that wanted the reason would
    /// have to re-derive the capability rule this machine owns, which is
    /// exactly what priority #2 forbids. So the reason ships beside the
    /// verdict, from the same shortfall computation.
    ///
    /// Note the deliberate asymmetry with `dns_provider_eligible` for an id
    /// outside `PROVIDERS`: that id is *ineligible* but has no row on screen,
    /// and there is no control to explain, so there is no reason to paint.
    pub fn dns_provider_ineligible_reason(&self, provider_id: String) -> Option<LocalizedText> {
        let (no_registrar, no_vps) = self.dns_provider_shortfalls(&provider_id)?;
        ineligibility_key(no_registrar, no_vps).map(LocalizedText::key)
    }

    /// Whether the dns_config page should render the WHOIS contact form. True
    /// only when the selected registrar requires a contact AND the domain is
    /// unregistered-but-buyable through it (`provider_status()` ==
    /// `UnregisteredBuyable`, i.e. the buy-domain path actually runs). Clients
    /// gate the contact form on this instead of re-combining
    /// `selected_registrar_requires_contact()` with `provider_status()`.
    pub fn should_show_contact_form(&self) -> bool {
        use crate::state::ProviderStatus;
        self.selected_registrar_requires_contact()
            && matches!(
                self.provider_status(),
                ProviderStatus::UnregisteredBuyable { .. }
            )
    }

    /// Whether the registrar-specific notes blurb (e.g. Porkbun's
    /// account-contact requirement) should be shown: only on the buy-domain
    /// path, and only when the selected provider declares a
    /// `registrar_notes_key`.
    pub fn should_show_registrar_notes(&self) -> bool {
        self.with_state(|s| {
            s.dns.buy_domain
                && s.dns
                    .selected_provider_id
                    .as_deref()
                    .and_then(|pid| PROVIDERS.iter().find(|p| p.id.as_str() == pid))
                    .map(|p| p.registrar_notes_key.is_some())
                    .unwrap_or(false)
        })
    }

    /// Whether the dns_config page should show the "no supported registrar
    /// carries .{tld}" message. True when the user must buy the domain
    /// (`buy_domain`) AND the handle-check probe found the domain available
    /// but not buyable via any supported registrar
    /// (`HandleCheckOutcome::DomainAvailable { buyable_via_provider: false }`).
    ///
    /// This is a TLD property knowable from the handle check alone: it does
    /// NOT require the user to first select + verify a provider, and it does
    /// not depend on *which* provider is selected. (Re-deriving it from
    /// `provider_status() == UnregisteredNotBuyable` — as iOS/web/android did
    /// before this getter — is both late (that status is `NotReady` until a
    /// provider is selected + verified) and provider-specific (it would
    /// mislabel a TLD that some registrar carries but the *selected* one does
    /// not).) The message text is about the TLD, so the handle-check semantic
    /// is the canonical one (`onboarding.md` § 4). Clients gate
    /// `dns-no-provider-message` on this getter instead of re-deriving it
    /// (priority #2; mirrors `should_show_contact_form` /
    /// `should_show_registrar_notes`).
    pub fn should_show_no_provider_message(&self) -> bool {
        use crate::snapshots::handle_check::HandleCheckOutcome;
        let buy_domain = self.with_state(|s| s.dns.buy_domain);
        buy_domain
            && matches!(
                self.handle_check.lock().unwrap().outcome,
                HandleCheckOutcome::DomainAvailable {
                    buyable_via_provider: false,
                    ..
                }
            )
    }

    /// Bottom-row Continue on the `nest_provisioning` page. Advances only when
    /// the provisioning snapshot is `Succeeded`; otherwise returns the current
    /// step unchanged so the click is a no-op.
    ///
    /// On the deferred-DNS path it transitions to `DnsPostInstructions`. On the
    /// standard path it lands on **`NatModeChoice`** (§ 3b-bis), exactly as a
    /// claim-code submit does — this page never exits straight to
    /// `Done`/`LoggedIn` (`docs/goal/behavior/onboarding.md` § 6, ratified
    /// 2026-08-29). `Succeeded` there means the box is built *and claimed* (the
    /// `Online` claiming substep, `run_provisioning_claim`), so the § 3b-bis /
    /// § 3b-ter tail owns the exit from here on. The pre-ratification `LoggedIn` /
    /// `Done` exit signed the user in to a box **nobody had claimed** and then
    /// bounced them to a claim page asking for a code they never saw.
    ///
    /// The standard path additionally requires the claim to have actually
    /// completed. That is not belt-and-braces: the orchestrator marks the run
    /// `Succeeded` and notifies before returning, so an app that paints and takes
    /// a click inside the window before `set_claiming` reopens the step could
    /// otherwise Continue past an unclaimed box. The gate is the claim's own
    /// state, not a timing assumption.
    pub fn continue_from_provisioning(&self) -> OnboardingStep {
        use fauna_provisioning::progress::OverallStatus;
        let snap = self.provisioning.lock().unwrap().clone();
        if !matches!(snap.overall, OverallStatus::Succeeded) {
            return self.with_state(|s| s.step);
        }
        let deferred = self.with_state(|s| s.dns.set_up_later);
        if deferred {
            self.mutate(|s| {
                s.step = OnboardingStep::DnsPostInstructions;
                s.error_message = None;
            });
            return OnboardingStep::DnsPostInstructions;
        }
        if !self.with_state(|s| s.claim_completed) {
            return self.with_state(|s| s.step);
        }
        let nest_url = self.with_state(|s| {
            let domain = s.handle_domain().unwrap_or_default().to_string();
            format!("https://{domain}")
        });
        // `nest_url` is the box's *identity* URL — the domain, never the reach
        // address the claim dialled (§ 6 *Reaching the box*).
        self.reset_nat_mode_snapshot();
        *self.outcome.lock().unwrap() = None;
        self.mutate(|s| {
            s.nest_url = nest_url;
            s.step = OnboardingStep::NatModeChoice;
            s.error_message = None;
        });
        OnboardingStep::NatModeChoice
    }

    /// Continue button on the `dns_post_instructions` page. Sets
    /// `wizard_outcome()` to `AwaitingManualDns { nest_url, dns_records,
    /// claim_code }` and returns `OnboardingStep::Done`. Caller is
    /// responsible for navigating to the "Almost ready" surface.
    pub fn continue_from_dns_post_instructions(&self) -> OnboardingStep {
        let claim_code = self
            .provisioning
            .lock()
            .unwrap()
            .result
            .as_ref()
            .map(|r| r.claim_code.clone())
            .unwrap_or_default();
        let (nest_url, dns_records) =
            self.with_state(|s| (s.nest_url.clone(), s.dns_records.clone()));
        // Prime the "Almost ready" surface so the client renders the records
        // immediately on this exit (same snapshot the relaunch path seeds).
        *self.awaiting_manual_dns.lock().unwrap() = AwaitingManualDnsSnapshot {
            state: AwaitingDnsState::Pending,
            dns_records: dns_records.clone(),
            message: LocalizedText {
                key: crate::snapshots::awaiting_manual_dns::resting_message_key(&dns_records)
                    .into(),
                args: Default::default(),
            },
        };
        *self.outcome.lock().unwrap() = Some(WizardOutcome::AwaitingManualDns {
            nest_url,
            dns_records,
            claim_code,
        });
        self.mutate(|s| {
            s.step = OnboardingStep::Done;
            s.error_message = None;
        });
        OnboardingStep::Done
    }

    /// Snapshot for the post-provisioning "Almost ready" surface. Pure read;
    /// cheap clone. The client renders this on every observer tick while
    /// `wizard_outcome()` is `AwaitingManualDns`. Per
    /// `docs/goal/behavior/onboarding.md` § "Wizard exit handling".
    pub fn awaiting_manual_dns_snapshot(&self) -> AwaitingManualDnsSnapshot {
        self.awaiting_manual_dns.lock().unwrap().clone()
    }

    /// The deferred-DNS records as the exact JSON the awaiting-DNS slot carries:
    /// `serde_json::to_string(&dns_records)` over the wizard's
    /// `Vec<DnsRecordPlain>` — the `dns_records_json` field of
    /// `fauna_launch_machine::AwaitingDnsRecord`.
    ///
    /// **Exists so no non-Rust client ever hand-rolls that JSON.** The seeder
    /// (`seed_awaiting_manual_dns`) parses it back with serde, which expects
    /// serde's field names (`record_type`, …) — but the UniFFI and WASM bindings
    /// expose the record as `recordType`. A client-side `Gson`/`JSON.stringify`
    /// of the *bound* type therefore produces JSON that deserializes to an empty
    /// list, silently losing the records the user still has to add at their
    /// registrar — a failure that only shows up after a relaunch. Rust clients
    /// call `serde_json::to_string` directly and get byte-identical output.
    pub fn awaiting_dns_records_json(&self) -> String {
        let records = self.awaiting_manual_dns.lock().unwrap().dns_records.clone();
        serde_json::to_string(&records).unwrap_or_else(|_| "[]".into())
    }

    /// The records the user must add at their registrar, formatted for display —
    /// one line per record. Every app renders the "Almost ready" records from
    /// this, and copies *this* to the clipboard, so the label and the copy button
    /// can never disagree and all seven apps give the same instruction.
    /// See [`crate::snapshots::awaiting_manual_dns::format_dns_records`].
    pub fn awaiting_dns_records_text(&self) -> String {
        let records = self.awaiting_manual_dns.lock().unwrap().dns_records.clone();
        crate::snapshots::awaiting_manual_dns::format_dns_records(&records)
    }

    /// Whether the "Almost ready" surface's "Copy all" button has anything to
    /// copy. False in the records-less mode — the resumed standard path, whose
    /// DNS was ours to write — where [`Self::awaiting_dns_records_text`] is
    /// empty and a click would copy nothing.
    ///
    /// A getter rather than a rule each app applies to `dns_records`, the same
    /// division `awaiting_dns_records_text` above already makes for the text
    /// itself: the app asks, the machine decides.
    /// See [`crate::snapshots::awaiting_manual_dns::copy_all_enabled`].
    pub fn awaiting_dns_copy_enabled(&self) -> bool {
        let records = self.awaiting_manual_dns.lock().unwrap().dns_records.clone();
        crate::snapshots::awaiting_manual_dns::copy_all_enabled(&records)
    }

    /// Whether the "Almost ready" surface's exit ("Use a different nest") may be
    /// taken now — off only while a claim is in flight. The app asks, the machine
    /// decides, exactly as for [`Self::awaiting_dns_copy_enabled`].
    /// See [`crate::snapshots::awaiting_manual_dns::fallthrough_enabled`].
    pub fn awaiting_dns_fallthrough_enabled(&self) -> bool {
        let state = self.awaiting_manual_dns.lock().unwrap().state.clone();
        crate::snapshots::awaiting_manual_dns::fallthrough_enabled(&state)
    }

    /// The "Almost ready" surface's explicit exit — **"Use a different nest"**
    /// (`onboarding-provisioning.md` § "Almost ready" surface → *Exit*): the way
    /// out for a box that will never answer (a `create_server` that failed after
    /// the slot was written, a box deleted at the provider), without which a
    /// resumed launch is pinned on a waiting page for ever.
    ///
    /// Clears the awaiting slot of the identity being onboarded (through the
    /// injected [`PendingProvisionStore`], addressed by that identity's secret —
    /// on an append run the active account is a different one) and lands the wizard
    /// at `HandleEntry` **holding the same identity**: the user is choosing a
    /// different nest, not a different self. The landing is `launch_retry`'s
    /// fallthrough (`seed_identity`) with the surface's state — outcome, snapshot,
    /// provisioning run, reach override — cleared behind it; the machine's
    /// in-memory pending-provision row survives, as it does every `reset()`, so
    /// choosing the SAME domain again resumes the box rather than minting a
    /// second one (§ 6 *The pending-provision slot*).
    ///
    /// The slot is cleared BEFORE the state moves: a crash between the two
    /// relaunches onto the surface with the exit still on it, which is a
    /// recoverable place, where the other order would land a slot-less identity
    /// on a wizard the user never asked for.
    ///
    /// No-op unless `wizard_outcome()` is `AwaitingManualDns` — a stray call must
    /// not retire a resumable box's slot — and while a claim is in flight
    /// ([`Self::awaiting_dns_fallthrough_enabled`]), which would race the claim's
    /// own nest-binding write.
    pub fn abandon_awaiting_manual_dns(&self) {
        if !matches!(
            self.wizard_outcome(),
            Some(WizardOutcome::AwaitingManualDns { .. })
        ) || !self.awaiting_dns_fallthrough_enabled()
        {
            return;
        }
        let secret = self.effective_secret();
        if let Some(secret) = &secret {
            let store = self
                .pending_provision
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(store) = store {
                store.clear_awaiting_dns(secret.clone());
            }
        }
        self.reset();
        if let Some(secret) = secret {
            self.seed_identity(secret);
        }
    }

    /// App-launch hydration for the "Almost ready" surface. Lands the wizard
    /// at the deferred-DNS exit from a previously-saved slot. The per-app
    /// glue calls `seed_identity(secret)` first (so signing works for the
    /// claim), then this with the persisted (nest_url, handle, dns_records,
    /// claim_code). Sets `wizard_outcome()` to `AwaitingManualDns` — exactly
    /// what the same-session `continue_from_dns_post_instructions` exit
    /// produces — so the client renders the surface identically whether it
    /// arrived this session or on relaunch. The client then polls
    /// `recheck_manual_dns()`. Mirrors `seed_pending_invite` /
    /// `seed_pending_encryption_mode_choice`. Per
    /// `docs/goal/behavior/onboarding.md` § "Wizard exit handling".
    ///
    /// Clients holding the slot's opaque `dns_records_json` should prefer
    /// [`Self::seed_awaiting_manual_dns_json`], which takes it verbatim.
    pub fn seed_awaiting_manual_dns(
        &self,
        nest_url: String,
        handle: String,
        dns_records: Vec<DnsRecordPlain>,
        claim_code: String,
    ) {
        self.seed_awaiting_manual_dns_record(AwaitingDnsRecord {
            nest_url,
            handle,
            dns_records_json: serde_json::to_string(&dns_records).unwrap_or_default(),
            claim_code,
            reach_ipv4: None,
            nest_actor_id: None,
        });
    }

    /// Relaunch hydration straight from the awaiting-DNS slot, taking the
    /// **whole record** as the launch flow read it
    /// (`fauna_launch_machine::AwaitingDnsRecord`) — the door every app's
    /// relaunch glue should walk through, so a field the slot grows (the
    /// reach address, the box's built-with identity) reaches the machine with
    /// no per-app change at all. The two field-taking forms above and below
    /// delegate here.
    ///
    /// Beyond what they seed, this re-holds the identity the box was built
    /// with (`record.nest_actor_id`, `security.md` § Transport trust — the
    /// *Client-provisioned box* row's "no TOFU window" holds across a
    /// relaunch only because the slot carries the root) as the first-contact
    /// root for the identity URL's host, and retains the row as the box this
    /// machine is mid-way through provisioning, so a "start over" onto the
    /// same domain after the relaunch resumes it rather than minting a fresh
    /// identity of it (§ 6 *The pending-provision slot*).
    pub fn seed_awaiting_manual_dns_record(&self, record: AwaitingDnsRecord) {
        let dns_records: Vec<DnsRecordPlain> =
            serde_json::from_str(&record.dns_records_json).unwrap_or_default();
        self.mutate(|s| {
            s.nest_url = record.nest_url.clone();
            s.handle = record.handle.clone();
            s.dns_records = dns_records.clone();
            s.step = OnboardingStep::Done;
            s.error_message = None;
        });
        *self.awaiting_manual_dns.lock().unwrap() = AwaitingManualDnsSnapshot {
            state: AwaitingDnsState::Pending,
            dns_records: dns_records.clone(),
            message: LocalizedText {
                key: crate::snapshots::awaiting_manual_dns::resting_message_key(&dns_records)
                    .into(),
                args: Default::default(),
            },
        };
        *self.outcome.lock().unwrap() = Some(WizardOutcome::AwaitingManualDns {
            nest_url: record.nest_url.clone(),
            dns_records,
            claim_code: record.claim_code.clone(),
        });
        if let Some(host) = crate::helpers::nest_host(&record.nest_url) {
            if let Some(id) = record
                .nest_actor_id
                .as_deref()
                .and_then(decode_nest_actor_id_hex)
            {
                self.hold_first_contact_identity(&host, id);
            }
            // **Re-arm the reach** (`onboarding.md` § "Almost ready" surface,
            // *Reach*): the box answers at the slot's address now and by its
            // domain only much later, so the surface's poll — and the claim
            // behind it — dial the address the run captured. Without this the
            // resumed surface waits on DNS propagation for a box that has been
            // up since cloud-init finished, which on a Hetzner-published zone
            // is a 30 min–5 h wait for something already reachable.
            //
            // A slot with no address (a crash between the mint and
            // `create_server` returning) arms
            // nothing and dials `nest_url`, exactly as before — the doc's own
            // fallback.
            if let Some(ipv4) = record.reach_ipv4.as_deref() {
                self.hold_reach_override(&host, ipv4);
            }
        }
        self.retain_pending_provision(record);
        self.observer.on_changed();
    }

    /// Relaunch hydration straight from the awaiting-DNS slot, taking the slot's
    /// opaque `dns_records_json` verbatim.
    ///
    /// The companion to [`Self::awaiting_dns_records_json`]: together they keep
    /// the record list **opaque to the client on both ends**, so a non-Rust
    /// client never builds or parses that JSON. That is what stops the bindings'
    /// camelCase (`recordType`) from silently round-tripping through serde's
    /// snake_case (`record_type`) to an *empty* list — a failure that would only
    /// surface after a relaunch, as an "Almost ready" page listing no records.
    ///
    /// A corrupt slot degrades to an empty record list rather than a panic: the
    /// user still reaches the surface (and can re-run the DNS step) instead of
    /// crashing on launch.
    pub fn seed_awaiting_manual_dns_json(
        &self,
        nest_url: String,
        handle: String,
        dns_records_json: String,
        claim_code: String,
    ) {
        self.seed_awaiting_manual_dns_record(AwaitingDnsRecord {
            nest_url,
            handle,
            dns_records_json,
            claim_code,
            reach_ipv4: None,
            nest_actor_id: None,
        });
    }
}

// ── DNS-provider eligibility: the one computation behind both answers ───────
//
// Deliberately OUTSIDE the `uniffi::export` block above: these are the shared
// internals of `dns_provider_eligible` / `dns_provider_ineligible_reason`, not
// app-facing API. Keeping the rule in one place is the point — a verdict and an
// explanation that can disagree is worse than no explanation at all.

impl OnboardingMachine {
    /// The two capability shortfalls for `provider_id` under the current DNS
    /// choices: `(wants a registrar it hasn't got, wants a VPS host it hasn't
    /// got)`. `None` for an id that is not a known provider.
    fn dns_provider_shortfalls(&self, provider_id: &str) -> Option<(bool, bool)> {
        let p = PROVIDERS.iter().find(|p| p.id.as_str() == provider_id)?;
        Some(self.with_state(|s| {
            (
                s.dns.buy_domain && !p.capabilities.contains(&Capability::Registrar),
                s.dns.same_provider_for_vps && !p.capabilities.contains(&Capability::Vps),
            )
        }))
    }
}

// ── VPS Continue gating: the one computation behind both answers ───────────

/// What the vps_config page is still missing before Continue means anything.
/// Ordered as the user meets them — a provider is picked, its credentials are
/// verified, and only then does the provider's API yield the locations and
/// plans the last two steps choose from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VpsShortfall {
    NoProvider,
    Unverified,
    NoLocation,
    NoServerType,
}

impl OnboardingMachine {
    /// The first thing standing between the current VPS choices and Continue,
    /// or `None` when nothing does. Sole source for both
    /// [`Self::can_continue_vps`] and [`Self::vps_continue_blocked_reason`],
    /// which is what keeps a live button from ever coexisting with an
    /// explanation of why it is dead (or vice versa).
    fn vps_continue_shortfall(&self) -> Option<VpsShortfall> {
        self.with_state(|s| {
            if s.vps.selected_provider_id.is_none() {
                Some(VpsShortfall::NoProvider)
            } else if !s.vps.verified {
                Some(VpsShortfall::Unverified)
            } else if s.vps.selected_location_id.is_none() {
                Some(VpsShortfall::NoLocation)
            } else if s.vps.selected_server_type_id.is_none() {
                Some(VpsShortfall::NoServerType)
            } else {
                None
            }
        })
    }
}

/// Which explainer a pair of shortfalls earns — total over all four
/// combinations, including the neither-capability case no shipped provider
/// reaches today (`providers.yaml` is generated data, and it grows).
/// The provider zone that holds `domain`: the zone that equals it, or the
/// longest zone it is a subdomain of (`example.com` holds `box.example.com`;
/// with both `example.com` and `eu.example.com` on the account,
/// `box.eu.example.com` belongs to the latter). Case-insensitive, and a
/// trailing dot on either side is ignored. One rule for the two readers that
/// must agree — which zone the wizard publishes into (`verify_dns`) and
/// whether the page says the provider has the domain (`provider_status`).
fn covering_zone<'a>(
    domain: &str,
    zones: &'a [fauna_provisioning::dns::DnsZone],
) -> Option<&'a fauna_provisioning::dns::DnsZone> {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    zones
        .iter()
        .filter(|z| {
            let zone = z.name.trim_end_matches('.').to_ascii_lowercase();
            !zone.is_empty() && (domain == zone || domain.ends_with(&format!(".{zone}")))
        })
        .max_by_key(|z| z.name.trim_end_matches('.').len())
}

fn ineligibility_key(no_registrar: bool, no_vps: bool) -> Option<&'static str> {
    match (no_registrar, no_vps) {
        (false, false) => None,
        (true, false) => Some("onboarding.dns_config.ineligible_needs_registrar"),
        (false, true) => Some("onboarding.dns_config.ineligible_needs_vps"),
        (true, true) => Some("onboarding.dns_config.ineligible_needs_registrar_and_vps"),
    }
}

// ── Handle-check orchestrator ───────────────────────────────────────────────

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl OnboardingMachine {
    pub async fn start_handle_check(&self, handle: String) {
        self.cancel.raise(); // cancel any in-flight
        self.cancel.reset();
        self.set_current_handle(handle.clone());
        // Reset to Idle, then walk phases.
        *self.handle_check.lock().unwrap() = HandleCheckSnapshot::idle();
        self.state.lock().unwrap().handle_enclosing_zone = None;
        self.observer.on_changed();
        self.run_handle_check_phases(handle).await;
    }

    async fn run_handle_check_phases(&self, handle: String) {
        use crate::snapshots::{HandleCheckOutcome, HandleCheckPhase};

        // Phase 1: Parse format.
        self.set_phase(
            HandleCheckPhase::Parsing,
            "handle_check.phase.parsing",
            HashMap::new(),
        );
        if self.cancel.is_raised() {
            return;
        }
        let Some(domain) = parse_handle_domain(&handle) else {
            self.complete(
                HandleCheckOutcome::FormatInvalid,
                "handle_check.outcome.format_invalid",
                HashMap::new(),
            );
            return;
        };

        // Resolve the handle's domain to a nest probe target. Local targets
        // (localhost / *.localhost / IP literal / host:port) denote a nest the
        // user runs themselves — there is no registerable domain to look up —
        // so they skip the DNS / TLD / price probes and hit the nest directly.
        // See docs/goal/behavior/onboarding.md §2 "Local / loopback targets".
        let mut target = fauna_provisioning::probe::resolve_handle_domain_with_local_port(
            &domain,
            *self
                .local_nest_port
                .read()
                .unwrap_or_else(|e| e.into_inner()),
        );
        // The "nest" override (E2E only) redirects nest HTTP at a fake/test
        // nest; when present, probe it directly and skip DNS too. The override
        // must NOT leak into persisted outcome data, so `state.nest_url` always
        // records the *resolved* URL (`target.base_url`), never the override.
        let nest_override = self.provider_base_url("nest".into());
        let skip_dns = !target.is_public_dns_name || nest_override.is_some();

        if !skip_dns {
            // Phase 2: DNS NS lookup.
            self.set_phase(
                HandleCheckPhase::DnsLookup,
                "handle_check.phase.dns_lookup",
                HashMap::new(),
            );
            if self.cancel.is_raised() {
                return;
            }
            let (dns, enclosing_zone, in_zone_address) =
                match fauna_provisioning::probe::dns_ns_probe(&self.http, &domain).await {
                    Ok(p) => (p.lookup, p.enclosing_zone, p.in_zone_address),
                    Err(e) => {
                        return self.complete_probe_error(
                            HandleCheckPhase::DnsLookup,
                            true,
                            e.to_string(),
                        );
                    }
                };
            if !dns.tld_valid {
                return self.complete(
                    HandleCheckOutcome::TldInvalid,
                    "handle_check.outcome.tld_invalid",
                    HashMap::from([(
                        "tld".to_string(),
                        domain.rsplit('.').next().unwrap_or_default().to_string(),
                    )]),
                );
            }
            // An in-zone name that already resolves is someone's host — most
            // often a nest serving `box.example.com` the admin signs back in to —
            // so it skips the available outcomes below and goes on to the nest
            // probe like any registered domain (`onboarding-provisioning.md` § 4).
            if !dns.has_ns
                && !in_zone_address
                && let Some(zone) = enclosing_zone
            {
                // No delegation of its own, inside a zone: a subdomain of a
                // domain someone holds — or, indistinguishably without
                // public-suffix knowledge, a name under a multi-label suffix
                // (`foo.co.uk`). The copy names both readings; Continue lands
                // with `buy_domain` off so every DNS provider stays selectable
                // (`onboarding-provisioning.md` § 4). No price lookup: a
                // subdomain has no registration price, and a registrar quotes
                // a buyable one after verify.
                let buyable = self.is_buyable_via_provider(&domain);
                self.state.lock().unwrap().handle_enclosing_zone = Some(zone.clone());
                return self.complete(
                    HandleCheckOutcome::DomainAvailable {
                        buyable_via_provider: buyable,
                        price: None,
                    },
                    "handle_check.outcome.domain_available_inside_zone",
                    HashMap::from([
                        ("domain".to_string(), domain.clone()),
                        ("zone".to_string(), zone),
                    ]),
                );
            }
            if !dns.has_ns && !in_zone_address {
                // Phase 2a: optional price lookup.
                self.set_phase(
                    HandleCheckPhase::PriceLookup,
                    "handle_check.phase.price_lookup",
                    HashMap::new(),
                );
                if self.cancel.is_raised() {
                    return;
                }
                let price =
                    fauna_provisioning::probe::domain_search_proxy_price(&self.http, &domain).await;
                let buyable = self.is_buyable_via_provider(&domain);
                // Pick (key, args) together so each template's `{domain}` (+
                // `{price}` for the priced variant, `{tld}` for the not-buyable
                // one) interpolates on clients that RESOLVE the i18n template
                // (macos/ios `renderLocalizedText`); empty args render a literal
                // `{domain}`. `&price` is borrowed only to format the amount —
                // the value still moves into the outcome below.
                let mut args = HashMap::from([("domain".to_string(), domain.clone())]);
                let key = if let Some(quote) = &price {
                    args.insert(
                        "price".to_string(),
                        crate::helpers::format_price(
                            quote.registration_cents,
                            quote.currency.clone(),
                        ),
                    );
                    "handle_check.outcome.domain_available_priced"
                } else if !buyable {
                    args.insert(
                        "tld".to_string(),
                        domain.rsplit('.').next().unwrap_or_default().to_string(),
                    );
                    "handle_check.outcome.domain_available_not_buyable_via_provider"
                } else {
                    "handle_check.outcome.domain_available_unpriced"
                };
                return self.complete(
                    HandleCheckOutcome::DomainAvailable {
                        buyable_via_provider: buyable,
                        price,
                    },
                    key,
                    args,
                );
            }

            // Pillar B — SRV-based port reach. The domain has NS records (it fell
            // through the DomainAvailable branch above), so it is a registered
            // public domain that may serve its client-facing API on a non-standard
            // port; consult `_fauna._tcp.<domain>` so a clean `alice@domain` handle
            // stays *port-hidden* — the port is discovered, not typed. Only when
            // the handle gave no explicit port (an explicit port is the user's own
            // override — honor it); loopback / LAN / `.local` took the `skip_dns`
            // branch and have no public SRV zone. Mirrors
            // `fauna_core::resolve::resolve_full_url`: keep the handle's host,
            // apply only the SRV port (an absent record or a literal `443` ⇒
            // unchanged, port-hidden). The lookup is DoH-based (cross-platform:
            // wasm + native), unlike the native-only hickory `resolve_node_url`.
            let (host, typed_port) = fauna_core::resolve::parse_node_address(&target.base_url);
            if typed_port.is_none()
                && let Some(srv_port) =
                    fauna_provisioning::probe::fauna_srv_port(&self.http, &host).await
                && srv_port != 443
            {
                target.base_url = format!("https://{host}:{srv_port}");
            }
        }

        // The probe base: the E2E `nest` override wins (it redirects HTTP at a
        // fake/test nest); otherwise the resolved target URL (including any
        // SRV-discovered port). The override never leaks into `state.nest_url`.
        let probe_base = nest_override.unwrap_or_else(|| target.base_url.clone());

        // A nest to talk to (or a local target we assume is one). Record its
        // resolved base URL so the continue-routing / claim path reuses it
        // instead of re-deriving `https://{domain}` (wrong for a loopback
        // `http://…:3000` target).
        self.mutate(|s| s.nest_url = target.base_url.clone());

        // Phase 3: Nest health probe.
        self.set_phase(
            HandleCheckPhase::NestProbe,
            "handle_check.phase.nest_probe",
            HashMap::from([("domain".to_string(), domain.clone())]),
        );
        if self.cancel.is_raised() {
            return;
        }
        // One probe client for every target class — see [`nest_probe_client`].
        // A nest on a self-signed floor is normal, not exceptional: local targets
        // (bare IP / localhost / .local) always are, and a public-domain nest is
        // too until the claim teaches it its name. Branching on host-class here
        // made a native app unable to claim a fresh internet nest at all
        // (the TLS failure read as `RegisteredNoNest`). Reachability only —
        // authentication is the downstream channel binding.
        let health =
            fauna_provisioning::probe::nest_health_probe_at(&self.http_nest_probe, &probe_base)
                .await;
        use fauna_provisioning::probe::NestHealthState;
        match health.state {
            NestHealthState::ConnectionRefused => {
                let mut snap = HandleCheckSnapshot::idle();
                snap.phase = HandleCheckPhase::Complete;
                snap.outcome = HandleCheckOutcome::RegisteredNoNest;
                snap.message = LocalizedText {
                    key: "handle_check.outcome.registered_no_nest".into(),
                    args: HashMap::from([("domain".to_string(), domain.clone())]),
                };
                snap.continue_enabled = false; // user must check the checkbox
                snap.control_checkbox_visible = true;
                snap.control_checkbox_checked = false;
                *self.handle_check.lock().unwrap() = snap;
                self.observer.on_changed();
                return;
            }
            NestHealthState::Misbehaving { .. } | NestHealthState::Timeout => {
                return self.complete_probe_error(
                    HandleCheckPhase::NestProbe,
                    true,
                    "nest unreachable".into(),
                );
            }
            NestHealthState::MalformedBody => {
                return self.complete_probe_error(
                    HandleCheckPhase::NestProbe,
                    false,
                    "nest protocol mismatch".into(),
                );
            }
            NestHealthState::Reachable => { /* fall through */ }
        }

        // Phase 4: Challenge response — the silent sign-in. Runs the
        // `fauna.auth.{challenge,verify}` ceremony over the anonymous WS-RPC
        // connection (`NestApi::silent_challenge`, migrated off the HTTP twin
        // `POST /api/v1/auth/{challenge,verify}`):
        // the nest issues a nonce, the ceremony signs the domain-tagged
        // `AUTH_VERIFY_V2 ‖ actor_id ‖ nonce ‖ nest_id`, and
        // verify reports whether this secret is registered here (and under what
        // handle). No active secret → empty hex → `SecretInvalid` (effectively
        // "we couldn't auth").
        self.set_phase(
            HandleCheckPhase::ChallengeResponse,
            "handle_check.phase.challenge_response",
            HashMap::new(),
        );
        if self.cancel.is_raised() {
            return;
        }
        let secret = self.effective_secret().unwrap_or_default();
        use crate::nest_api::SilentChallengeOutcome;
        match self.nest_api.silent_challenge(&probe_base, &secret).await {
            SilentChallengeOutcome::Success(verify) => {
                // The nest returns the registered handle as a BARE localpart (it
                // stores localparts), so also compare against the typed handle's
                // localpart — otherwise a returning user signing in with their own
                // `localpart@domain` always reads as "handle differs". Empty
                // handle (unset on the nest) → `None`.
                let cur = if verify.handle.is_empty() {
                    None
                } else {
                    Some(verify.handle)
                };
                let typed_local = handle.split('@').next().unwrap_or(handle.as_str());
                let matches =
                    cur.as_deref() == Some(handle.as_str()) || cur.as_deref() == Some(typed_local);
                // `already_on_nest_handle_matches` greets with the typed
                // `{handle}`; `…_handle_differs` reports the `{domain}` plus the
                // registered `{old_handle}` (`cur`). Clone before `cur` moves
                // into the outcome below. Empty args would leave macos/ios
                // showing the literal placeholders.
                let (key, args) = if matches {
                    (
                        "handle_check.outcome.already_on_nest_handle_matches",
                        HashMap::from([("handle".to_string(), handle.clone())]),
                    )
                } else {
                    (
                        "handle_check.outcome.already_on_nest_handle_differs",
                        HashMap::from([
                            ("domain".to_string(), domain.clone()),
                            ("old_handle".to_string(), cur.clone().unwrap_or_default()),
                        ]),
                    )
                };
                self.complete(
                    HandleCheckOutcome::AlreadyOnNest {
                        handle_matches: matches,
                        current_handle: cur,
                    },
                    key,
                    args,
                );
            }
            SilentChallengeOutcome::NotRegistered => {
                // Discriminate "claimed nest, user unregistered" (→
                // invite_request) from "unclaimed nest" (→ claim_code) by
                // probing setup-status. Per `docs/goal/behavior/onboarding.md` §2:
                // `UnregisteredUnclaimedNest` is the unclaimed-nest
                // discriminator. A claimed nest has no path for a
                // not-yet-invited user other than asking the admin; an
                // unclaimed nest has no admin to ask, so the user must
                // claim it themselves.
                //
                // Failure modes:
                // - 200 with `claimed: true` → existing branch.
                // - 200 with `claimed: false` (or empty body) → claim_code.
                // - non-2xx / network failure → fall through to the existing
                //   claimed branch (the safer default — it doesn't try to
                //   make the user claim a nest that's actually claimed by
                //   someone else).
                // Use the NestApi trait so test fixtures can swap in a
                // FakeNestApi. The trait method takes the full base URL — we
                // reuse the resolved `probe_base` (honors scheme/port for
                // local targets and the E2E "nest" override) rather than
                // re-deriving `https://{domain}`.
                let (claimed, node_mode) = match self.nest_api.probe_setup_status(&probe_base).await
                {
                    Ok(status) => (status.claimed, status.node_mode),
                    // Treat "no answer" as "assume claimed" — the
                    // invite_request branch is graceful for users who
                    // arrive at a nest they don't have admin for, while
                    // the claim_code branch only works when there's a
                    // live claim-code file.
                    Err(_) => (true, None),
                };
                // Capture the resolved NAT-mode seed so `reset_nat_mode_snapshot`
                // can pre-select the `nat_mode_choice` page (onboarding.md
                // § 3b-bis).
                self.mutate(|s| s.node_mode_seed = node_mode);
                if claimed {
                    self.complete(
                        HandleCheckOutcome::NestRunningUserUnregistered,
                        "handle_check.outcome.user_unregistered",
                        HashMap::from([("domain".to_string(), domain.clone())]),
                    );
                } else {
                    self.complete(
                        HandleCheckOutcome::UnregisteredUnclaimedNest,
                        "handle_check.outcome.unregistered_unclaimed_nest",
                        HashMap::from([("domain".to_string(), domain.clone())]),
                    );
                }
            }
            // Reachability faults (server error, timeout, malformed reply,
            // disconnect, connect-refused, expired/consumed nonce) collapse to
            // the retryable bucket — client-side transient-vs-terminal
            // classification is unreliable, and a false-terminal is worse than
            // offering retry (onboarding.md lines 248–252). Subsumes the old
            // probe's `ServerError { transient: false }` cases.
            SilentChallengeOutcome::Transient { error: _ } => self.complete_probe_error(
                HandleCheckPhase::ChallengeResponse,
                true,
                "challenge error".into(),
            ),
            // The secret isn't a valid 32-byte Ed25519 key — terminal, not
            // retryable (the old probe's `SignatureMismatch`).
            SilentChallengeOutcome::SecretInvalid { error: _ } => self.complete_probe_error(
                HandleCheckPhase::ChallengeResponse,
                false,
                "signature mismatch".into(),
            ),
            // The nest booted a degraded "needs-update" mode
            // (fauna.nest.outdated) — terminal here; the fix is updating the
            // nest, not a retry (version-compatibility.md Dim 4).
            SilentChallengeOutcome::NeedsUpdate { message: _ } => self.complete_probe_error(
                HandleCheckPhase::ChallengeResponse,
                false,
                "nest outdated".into(),
            ),
            // The typed identity verdict (security.md § Pre-claim surfacing): `WsNestApi::core` graduates the
            // challenge's fresh connection and maps a pin-related failure (a
            // changed TOFU pin for a previously-pinned host) — or any
            // graduation failure while a first-contact root is held — to this
            // variant. Terminal here: the re-trust affordance is the LAUNCH
            // surface (`launch_identity_changed`), not a wizard probe, and a
            // retry can't change the verdict.
            SilentChallengeOutcome::IdentityChanged { .. } => self.complete_probe_error(
                HandleCheckPhase::ChallengeResponse,
                false,
                "nest identity changed".into(),
            ),
            // This identity was succeeded (`identity-succession.md`
            // § Propagation → *Own device fleet*). Terminal, and terminal in a
            // way no retry touches: the old key still signs valid bytes that
            // authorize nothing. The affordance that resolves it — "import the
            // new identity" naming the successor — belongs to the surfaces that
            // own an identity, i.e. the LAUNCH path and the `recovery_entry`
            // screen (`onboarding.md` § 1 Identity), not to a handle-check
            // probe, so this arm stops the probe honestly rather than routing.
            SilentChallengeOutcome::Superseded { .. } => self.complete_probe_error(
                HandleCheckPhase::ChallengeResponse,
                false,
                "identity superseded".into(),
            ),
            // A locked account (`login.md` § Silent Challenge). Terminal until
            // `locked_until`; the locked surface belongs to the LAUNCH path
            // (`devices.md` § The locked state), not a handle-check probe.
            SilentChallengeOutcome::Locked { .. } => self.complete_probe_error(
                HandleCheckPhase::ChallengeResponse,
                false,
                "account locked".into(),
            ),
        }
    }

    fn set_phase(
        &self,
        phase: crate::snapshots::HandleCheckPhase,
        msg_key: &str,
        args: HashMap<String, String>,
    ) {
        let mut snap = self.handle_check.lock().unwrap();
        snap.phase = phase;
        snap.outcome = crate::snapshots::HandleCheckOutcome::None;
        snap.message = LocalizedText {
            key: msg_key.into(),
            args,
        };
        snap.continue_enabled = false;
        drop(snap);
        self.observer.on_changed();
    }

    fn complete(
        &self,
        outcome: crate::snapshots::HandleCheckOutcome,
        msg_key: &str,
        args: HashMap<String, String>,
    ) {
        use crate::snapshots::{HandleCheckOutcome, HandleCheckPhase};
        let continue_enabled = matches!(
            outcome,
            HandleCheckOutcome::DomainAvailable { .. }
                | HandleCheckOutcome::AlreadyOnNest { .. }
                | HandleCheckOutcome::NestRunningUserUnregistered
                | HandleCheckOutcome::UnregisteredUnclaimedNest
        );
        let mut snap = self.handle_check.lock().unwrap();
        snap.phase = HandleCheckPhase::Complete;
        snap.outcome = outcome;
        snap.message = LocalizedText {
            key: msg_key.into(),
            args,
        };
        snap.continue_enabled = continue_enabled;
        drop(snap);
        self.observer.on_changed();
    }

    fn complete_probe_error(
        &self,
        phase: crate::snapshots::HandleCheckPhase,
        transient: bool,
        cause: String,
    ) {
        use crate::snapshots::HandleCheckOutcome;
        let key = if transient {
            "handle_check.error.transient"
        } else {
            "handle_check.error.terminal"
        };
        let mut snap = self.handle_check.lock().unwrap();
        snap.phase = crate::snapshots::HandleCheckPhase::Complete;
        snap.outcome = HandleCheckOutcome::ProbeError {
            phase,
            transient,
            cause: cause.clone(),
        };
        snap.message = LocalizedText {
            key: key.into(),
            args: [("cause".into(), cause)].into_iter().collect(),
        };
        snap.continue_enabled = false;
        drop(snap);
        self.observer.on_changed();
    }

    fn is_buyable_via_provider(&self, domain: &str) -> bool {
        // Simple TLD-set check against providers.yaml. For Plan 1, hardcode
        // the 4 known-supported registrar TLDs; refine later via providers.yaml.
        let Some(tld) = domain.rsplit('.').next() else {
            return false;
        };
        matches!(tld, "com" | "net" | "org" | "io" | "dev" | "app")
    }

    pub fn cancel_handle_check(&self) {
        self.cancel.raise();
        *self.handle_check.lock().unwrap() = HandleCheckSnapshot::idle();
        self.observer.on_changed();
    }

    pub fn set_control_checkbox(&self, checked: bool) {
        let mut snap = self.handle_check.lock().unwrap();
        if !snap.control_checkbox_visible {
            return;
        }
        snap.control_checkbox_checked = checked;
        snap.continue_enabled = checked;
        drop(snap);
        self.observer.on_changed();
    }

    pub async fn submit_handle_check_continue(&self) -> OnboardingStep {
        use crate::snapshots::HandleCheckOutcome;
        let snap = self.handle_check.lock().unwrap().clone();
        // Recovery mode (Q2-A, box-recovery.md § Recovery UI (step 4)): the
        // admin typed the handle/@domain of ANOTHER box they own, to reach a
        // surviving nest and read its synced deployment-seed map. Reachability
        // + owning the identity (AlreadyOnNest) is all `deploymentSeeds()`
        // needs — not registering on this nest — so land on the box-selection
        // hub instead of Done/LoggedIn. `state.nest_url` was already resolved
        // during the probe phase (`submit_handle`), so the stored nest URL is
        // ready for the recovery box-list read the moment the UI lands.
        if self.with_state(|s| s.recovery_intent)
            && matches!(snap.outcome, HandleCheckOutcome::AlreadyOnNest { .. })
        {
            let next = OnboardingStep::NestRecovery;
            self.state.lock().unwrap().step = next;
            self.observer.on_changed();
            return next;
        }
        let next = match snap.outcome {
            HandleCheckOutcome::DomainAvailable { .. } => {
                let mut s = self.state.lock().unwrap();
                // Off for a name inside a zone (a subdomain of a held domain):
                // the buy filter would hide the DNS host holding the parent
                // zone (onboarding-provisioning.md § 4).
                s.dns.buy_domain = s.handle_enclosing_zone.is_none();
                // domain_status() derives Unregistered from this same
                // DomainAvailable outcome (onboarding.md §4) — nothing to
                // persist here beyond buy_domain.
                OnboardingStep::DnsConfig
            }
            HandleCheckOutcome::RegisteredNoNest if snap.control_checkbox_checked => {
                let mut s = self.state.lock().unwrap();
                s.dns.buy_domain = false;
                // The user owns the domain (registered, no nest) and isn't
                // buying it. domain_status() derives RegisteredNoNest from
                // this same outcome, so provider_status() reports
                // RegisteredElsewhere and the buy-domain checkbox stays
                // disabled (per onboarding.md §4 / ui.yaml) with nothing
                // further to persist here.
                OnboardingStep::DnsConfig
            }
            HandleCheckOutcome::AlreadyOnNest { current_handle, .. } => {
                // Derive the nest base URL from the handle the USER TYPED — it
                // carries the `@domain` that the probe just resolved (via DoH;
                // scheme/port honored for local targets, e.g.
                // http://localhost:3000). The nest-returned `current_handle` is a
                // BARE localpart (the nest stores localparts — see
                // onboarding-auto-adds-handle-domain), so it has no domain to
                // resolve: using it yields an empty nest_url and a dead WS
                // connection on sign-in. `current_handle` is kept only as the
                // logged-in identity label. Independent of whether the probe
                // phases ran, so snapshot-injection paths work too.
                let typed_handle = self.current_handle();
                let local_nest_port = *self
                    .local_nest_port
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                let nest_url = parse_handle_domain(&typed_handle)
                    .map(|d| {
                        fauna_provisioning::probe::resolve_handle_domain_with_local_port(
                            &d,
                            local_nest_port,
                        )
                        .base_url
                    })
                    .unwrap_or_default();
                let handle = current_handle.unwrap_or(typed_handle);
                // `Suppress`: this user is ALREADY on this box, so § 3b-ter's
                // "joining user's first login" is behind them — this is a
                // second device or a later launch, and the same trust is
                // grantable any time from the Nests page.
                self.leave_for_logged_in(nest_url, handle, TrustOffer::Suppress)
            }
            // Invite redemption proceeds with **no consent branch** — the
            // plaintext-mode consent page is retired without replacement
            // (`onboarding.md` § 3c): there is no deployment storage posture to
            // consent to. What a box can read is per-user, visible as its
            // capability grants on the Nests page.
            HandleCheckOutcome::NestRunningUserUnregistered => OnboardingStep::InviteRequest,
            HandleCheckOutcome::UnregisteredUnclaimedNest => {
                // Per `docs/goal/behavior/onboarding.md` §3a: record the
                // resolved nest base URL (scheme/port honored for local
                // targets, e.g. http://localhost:3000) so
                // `wizard_submit_claim_code` POSTs to the right place.
                let domain = parse_handle_domain(&self.current_handle()).unwrap_or_default();
                if !domain.is_empty() {
                    let port = *self
                        .local_nest_port
                        .read()
                        .unwrap_or_else(|e| e.into_inner());
                    let base = fauna_provisioning::probe::resolve_handle_domain_with_local_port(
                        &domain, port,
                    )
                    .base_url;
                    self.state.lock().unwrap().nest_url = base;
                }
                // Re-seed the page, exactly as both
                // `navigate_to_claim_code_for_known_nest*` helpers do — this is
                // the third door into `ClaimCode` and it must hand the user the
                // same submittable page they do. A native app holds ONE
                // long-lived machine per process, so the TERMINAL `Claimed`
                // snapshot of an earlier claim (`submit_enabled == false`)
                // otherwise survives into this one and renders Submit
                // permanently dead — the same shape as the `provisioning`
                // snapshot bug `reset()` documents, one page over.
                *self.claim_code.lock().unwrap() = crate::snapshots::ClaimCodeSnapshot::idle();
                OnboardingStep::ClaimCode
            }
            _ => self.state.lock().unwrap().step,
        };
        self.state.lock().unwrap().step = next;
        self.observer.on_changed();
        next
    }
}

// ── Invite-request orchestrator (handle-check wizard, Plan 1+) ────────────
//
// NOTE: The old `submit_invite_request(message)` (Plan 0 API) is kept below
// in the "Invite request stage" section for the existing WASM/Linux bindings.
// These new methods are the snapshot-based wizard variants.

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl OnboardingMachine {
    pub async fn wizard_submit_invite_request(&self) -> OnboardingStep {
        use crate::snapshots::{ErrorContext, InviteRequestState};
        // A denied row must be withdrawn before a new request is accepted — the
        // nest refuses a submit while ANY row exists for this actor, which is
        // what left a denied requester with no way back in. Read the flag before
        // the `Submitting` transition overwrites the state that carries it.
        let resubmitting_after_denial = matches!(
            self.invite_request.lock().unwrap().state,
            InviteRequestState::Denied { .. }
        );

        // Move snapshot to Submitting.
        self.invite_request.lock().unwrap().state = InviteRequestState::Submitting;
        self.observer.on_changed();

        let handle = self.current_handle();
        let nest_url = self.effective_nest_url();
        let secret = match self.effective_secret() {
            Some(s) => s,
            None => {
                return self.invite_error(ErrorContext::Submitting, false, "no identity".into());
            }
        };

        if resubmitting_after_denial {
            // Best-effort by design: the kind is idempotent, and the only
            // outcome that matters is whether the submit below is accepted. A
            // cancel that fails for a row the nest already dropped would
            // otherwise block a resubmit the nest would have taken.
            let _ = self
                .nest_api
                .cancel_invite_request(&nest_url, &secret)
                .await;
        }

        let signing_key = match parse_signing_key(&secret) {
            Some(k) => k,
            None => {
                return self.invite_error(
                    ErrorContext::Submitting,
                    false,
                    "invalid identity secret".into(),
                );
            }
        };
        let actor_id_bytes = signing_key.verifying_key().to_bytes();
        let actor_id_hex = hex::encode(actor_id_bytes);
        let timestamp_ms = Timestamp::now_millis();
        // Free-text message field is reserved for a future "tell the admin
        // why you want in" UX. The wizard's current page doesn't surface it,
        // so we send the empty string — matches the nest's `default = ""`.
        let user_message = "";
        // The BARE local part, as `redeem_invite`'s register does: the submit
        // door's `validate_handle` rejects '@' before anything else, and its
        // signature covers the handle alone (no domain). Sending the whole
        // wizard handle was refused as `invalid_request` for every typed
        // `user@domain`. Pinned by
        // `invite_submit_sends_the_bare_local_part_signed_over_it`.
        let requested_handle = handle_local_part(&handle).to_string();
        let signed = fauna_protocol::invite::invite_submit_signed_message(
            &actor_id_bytes,
            &requested_handle,
            user_message,
            timestamp_ms,
        );
        let signature_hex = sign_bytes(&secret, &signed);

        let body = crate::nest_api::InviteRequestBody {
            actor_id: actor_id_hex,
            handle: requested_handle,
            message: user_message.to_string(),
            timestamp: timestamp_ms,
            signature: signature_hex,
            age_claim: self.age_claim_to_send(),
        };
        let next_state = match self.nest_api.submit_invite_request(&nest_url, body).await {
            Ok(resp) => state_from_resp(resp),
            Err(crate::nest_api::InviteRequestError::Closed { cause }) => {
                InviteRequestState::Error {
                    transient: false,
                    context: ErrorContext::Submitting,
                    cause,
                }
            }
            Err(crate::nest_api::InviteRequestError::RateLimited { cause }) => {
                InviteRequestState::Error {
                    transient: false,
                    context: ErrorContext::Submitting,
                    cause,
                }
            }
            Err(crate::nest_api::InviteRequestError::Transient { cause }) => {
                // The trait's `Transient` variant covers both the server
                // 5xx/internal case and the connection-failure case (the
                // WS-RPC mapping funnels both here). For the wizard, both
                // are user-facing "try again later" — render as transient
                // with the underlying cause.
                let user_cause = if cause == "network" {
                    "network".to_string()
                } else {
                    cause
                };
                InviteRequestState::Error {
                    transient: true,
                    context: ErrorContext::Submitting,
                    cause: user_cause,
                }
            }
            Err(crate::nest_api::InviteRequestError::Malformed { cause }) => {
                InviteRequestState::Error {
                    transient: false,
                    context: ErrorContext::Submitting,
                    cause,
                }
            }
            // The nest already holds this key — for a user who reached this
            // page, a suspended one (`login.md` § Errors). Terminal, with a
            // key-shaped sentinel `message_for` renders as its own sentence.
            Err(crate::nest_api::InviteRequestError::AlreadyRegistered) => {
                InviteRequestState::Error {
                    transient: false,
                    context: ErrorContext::Submitting,
                    cause: crate::snapshots::ALREADY_REGISTERED_CAUSE.into(),
                }
            }
            Err(crate::nest_api::InviteRequestError::NotFound) =>
            // POST /invite-requests doesn't return 404 — but the
            // exhaustiveness of the enum forces an arm. Surface as
            // transient with a diagnostic cause; matches the legacy
            // `format!("status {}", r.status())` fallback shape.
            {
                InviteRequestState::Error {
                    transient: false,
                    context: ErrorContext::Submitting,
                    cause: "status 404".into(),
                }
            }
            // The typed identity verdict (security.md § Pre-claim surfacing):
            // terminal, never "try again later".
            Err(crate::nest_api::InviteRequestError::IdentityMismatch { reason }) => {
                InviteRequestState::Error {
                    transient: false,
                    context: ErrorContext::Submitting,
                    cause: reason,
                }
            }
        };

        // NOTE: per-app glue is responsible for persisting PendingReview
        // results to its long-term store (analogous to how it persists
        // identity from confirm_*_identity). The wizard does not call any
        // store directly.

        let mut snap = self.invite_request.lock().unwrap();
        snap.state = next_state.clone();
        // Continue is the OOB-code path's control and nothing else
        // (`onboarding.md` § 3, `invite-request-continue-button`): it is
        // **disabled during `PendingReview`**, because that journey advances by
        // polling, never by a continue-exit.
        snap.continue_enabled = continue_enabled_for(&snap.out_of_band_code_state);
        snap.recheck_visible = matches!(next_state, InviteRequestState::PendingReview { .. });
        drop(snap);
        self.observer.on_changed();

        // The wizard STAYS on this page — there is no exit for a pending review.
        // The per-app glue persists the slot on this return (the only write
        // moment) and starts polling; § The pending-invite surface.
        self.state.lock().unwrap().step
    }

    pub async fn recheck_invite_status(&self) -> OnboardingStep {
        use crate::snapshots::{ErrorContext, InviteRequestState};
        // Recheck is only meaningful from PendingReview; otherwise return. Take
        // the request id here: the `Rechecking` transition below overwrites the
        // state that carries it, and the not-found probe needs it to restore
        // this pending state when it cannot reach a verdict.
        let pending_request_id = match &self.invite_request.lock().unwrap().state {
            InviteRequestState::PendingReview { request_id, .. } => request_id.clone(),
            _ => return self.state.lock().unwrap().step,
        };
        let nest_url = self.effective_nest_url();
        let secret = match self.effective_secret() {
            Some(s) => s,
            None => {
                return self.invite_error(ErrorContext::Rechecking, false, "no identity".into());
            }
        };
        let actor_id_hex = derive_pubkey(&secret);
        if actor_id_hex.is_empty() {
            return self.invite_error(
                ErrorContext::Rechecking,
                false,
                "invalid identity secret".into(),
            );
        }
        self.invite_request.lock().unwrap().state = InviteRequestState::Rechecking;
        self.observer.on_changed();

        let recheck = self
            .nest_api
            .recheck_invite_request(&nest_url, &actor_id_hex)
            .await;

        // `NotFound` is the APPROVAL signal, not an error: the admin approve
        // creates the account and deletes the request row (`admin.md` § Section
        // 1), so the row's absence is how admission reaches an anonymous poller.
        // It is ambiguous only with a cancelled/purged request, so ask the one
        // authority that can tell them apart before rendering anything.
        if matches!(recheck, Err(crate::nest_api::InviteRequestError::NotFound)) {
            return self
                .resolve_not_found_recheck(&nest_url, &secret, &pending_request_id)
                .await;
        }

        let next_state = match recheck {
            Ok(resp) => state_from_resp(resp),
            Err(crate::nest_api::InviteRequestError::NotFound) => {
                unreachable!("handled by the registered-probe branch above")
            }
            Err(crate::nest_api::InviteRequestError::Transient { cause }) => {
                InviteRequestState::Error {
                    transient: true,
                    context: ErrorContext::Rechecking,
                    cause,
                }
            }
            Err(crate::nest_api::InviteRequestError::Malformed { cause }) => {
                InviteRequestState::Error {
                    transient: false,
                    context: ErrorContext::Rechecking,
                    cause,
                }
            }
            // Recheck doesn't return 403/429 in practice; treat as terminal
            // error with the underlying cause.
            Err(crate::nest_api::InviteRequestError::Closed { cause })
            | Err(crate::nest_api::InviteRequestError::RateLimited { cause }) => {
                InviteRequestState::Error {
                    transient: false,
                    context: ErrorContext::Rechecking,
                    cause,
                }
            }
            // The typed identity verdict (security.md § Pre-claim surfacing):
            // terminal — the pending-invite poll must stop, not spin on a
            // MITM signal.
            Err(crate::nest_api::InviteRequestError::IdentityMismatch { reason }) => {
                InviteRequestState::Error {
                    transient: false,
                    context: ErrorContext::Rechecking,
                    cause: reason,
                }
            }
            // The recheck kind never answers `actor_exists` (an admitted
            // actor's row is gone, which is the `NotFound` probe above); the
            // arm is exhaustiveness only, terminal like its submit twin.
            Err(crate::nest_api::InviteRequestError::AlreadyRegistered) => {
                InviteRequestState::Error {
                    transient: false,
                    context: ErrorContext::Rechecking,
                    cause: crate::snapshots::ALREADY_REGISTERED_CAUSE.into(),
                }
            }
        };
        let mut snap = self.invite_request.lock().unwrap();
        snap.state = next_state.clone();
        // Same rule as the submit return: Continue tracks the OOB code alone.
        snap.continue_enabled = continue_enabled_for(&snap.out_of_band_code_state);
        snap.recheck_visible = matches!(next_state, InviteRequestState::PendingReview { .. });
        drop(snap);
        self.observer.on_changed();
        self.state.lock().unwrap().step
    }

    /// Disambiguate a `NotFound` recheck by asking whether this identity is now
    /// *registered* on the nest — the "approval is detected as admission" rule
    /// (`onboarding.md` § The pending-invite surface).
    ///
    /// The request row is gone either because the admin approved it (the approve
    /// creates the account, then deletes the row) or because it was cancelled /
    /// purged. Nothing in the row's absence separates those, and an anonymous
    /// poller has no notification channel to be told which happened (every
    /// notification plane is keyed on a bearer-proven `actor_id` this caller does
    /// not have until approval creates it). So we run the same
    /// `fauna.auth.{challenge,verify}` ceremony the silent sign-in uses, with the
    /// identity we already hold:
    ///
    /// - **registered** → that verify *is* the login. Route to `LoggedIn`/`Done`
    ///   exactly like a redeem success — no "Approved, press Continue"
    ///   interstitial, mirroring the awaiting-DNS surface's auto-proceed.
    /// - **not registered** → the request is genuinely gone: terminal
    ///   `invite.error.not_found`, on which the per-app glue deletes the slot.
    /// - **couldn't tell** (unreachable, degraded nest, unusable secret) → stay
    ///   `PendingReview` and let the next poll tick ask again. Deliberately NOT
    ///   an `Error` state: `recheck_invite_status` only proceeds from
    ///   `PendingReview`, so writing any error here would wedge the poll
    ///   permanently — a dropped connection would strand a live request behind a
    ///   terminal, slot-deleting screen.
    async fn resolve_not_found_recheck(
        &self,
        nest_url: &str,
        secret: &str,
        pending_request_id: &str,
    ) -> OnboardingStep {
        use crate::nest_api::SilentChallengeOutcome;
        use crate::snapshots::{ErrorContext, InviteRequestState};

        match self.nest_api.silent_challenge(nest_url, secret).await {
            SilentChallengeOutcome::Success(_) => {
                // `state.nest_url`, NOT `effective_nest_url()`: this URL is
                // persisted into the WizardOutcome and surfaced to per-app glue
                // as the nest's identity. The provider_base_urls override is for
                // request targets only — leaking it here would put a test-cloud
                // URL into production identity-store records.
                let nest_url = self.state.lock().unwrap().nest_url.clone();
                // The wizard's typed handle, not the verify's bare localpart:
                // the identity store records `localpart@domain`, and the nest
                // stores localparts (the same asymmetry the handle-check's
                // `typed_local` comparison exists for).
                let handle = self.current_handle();
                // `Offer`: an approved join request is a JOIN — this poll tick
                // is the moment the user lands on a nest they were not on
                // (§ 3b-ter). Parking here cannot strand the poll:
                // `recheck_invite_status` proceeds only from `PendingReview`,
                // which this transition has left.
                self.leave_for_logged_in(nest_url, handle, TrustOffer::Offer)
            }
            SilentChallengeOutcome::NotRegistered => self.invite_error(
                ErrorContext::Rechecking,
                false,
                "invite.error.not_found".into(),
            ),
            // Restore the pending state the recheck's `Rechecking` spinner
            // replaced, so the page renders as pending and the poll continues.
            _ => {
                {
                    let mut snap = self.invite_request.lock().unwrap();
                    if let InviteRequestState::Rechecking = snap.state {
                        snap.state = InviteRequestState::PendingReview {
                            request_id: pending_request_id.to_string(),
                            last_checked_ms: Timestamp::now_millis(),
                        };
                        snap.recheck_visible = true;
                    }
                }
                self.observer.on_changed();
                self.state.lock().unwrap().step
            }
        }
    }

    pub async fn verify_oob_invite_code(&self, code: String) {
        use crate::snapshots::OobCodeState;
        self.invite_request.lock().unwrap().out_of_band_code_state = OobCodeState::Verifying;
        self.observer.on_changed();
        let nest_url = self.effective_nest_url();
        let next = match self.nest_api.verify_invite_code(&nest_url, &code).await {
            Ok(v) => OobCodeState::Valid {
                invite_id: v.invite_id,
                supervised_by: v.supervised_by,
            },
            Err(crate::nest_api::InviteCodeError::Invalid { reason }) => {
                OobCodeState::Invalid { reason }
            }
            Err(crate::nest_api::InviteCodeError::Malformed { cause }) => {
                OobCodeState::Error { cause }
            }
            Err(crate::nest_api::InviteCodeError::Transient { cause }) => {
                OobCodeState::Error { cause }
            }
            // The typed identity verdict (security.md § Pre-claim surfacing):
            // the same visible error state, with the verdict's own reason.
            Err(crate::nest_api::InviteCodeError::IdentityMismatch { reason }) => {
                OobCodeState::Error { cause: reason }
            }
        };
        {
            let mut req = self.invite_request.lock().unwrap();
            req.out_of_band_code_state = next;
            // continue_enabled must track the SAME condition redeem_invite()
            // routes on — recomputed here, not just set true on Valid, so a
            // later re-check that comes back Invalid/Error correctly reverts it.
            // Since the Approved/PendingReview disjuncts retired with the
            // continue-exit, that condition is now the OOB code alone.
            req.continue_enabled = continue_enabled_for(&req.out_of_band_code_state);
        }
        self.observer.on_changed();
    }

    pub async fn redeem_invite(&self) -> OnboardingStep {
        use crate::snapshots::OobCodeState;
        let snap = self.invite_request.lock().unwrap().clone();
        let nest_url = self.effective_nest_url();
        let handle = self.current_handle();

        // The OOB arm is the live one: the user pasted a server-issued invite
        // code, which reaches `register` in the body.
        //
        // ⚠ The former `Approved` arm here was DOUBLY dead and retired with the
        // variant (2026-08-12): an approve creates the account first, so the
        // code-less register it sent was refused as `ActorAlreadyRegistered` —
        // and no live nest ever served `Approved` to reach it anyway. Approval
        // is handled by the recheck's registered-probe instead, so redeem is
        // now the out-of-band code path and nothing else.
        let oob_invite_code = match &snap.out_of_band_code_state {
            OobCodeState::Valid { invite_id, .. } => Some(invite_id.clone()),
            _ => return self.state.lock().unwrap().step, // nothing to do
        };

        // No setup-status probe here, and no consent branch: invite redemption
        // proceeds straight to `register`. The plaintext-mode consent page is
        // retired without replacement (`onboarding.md` § 3c) — there is no
        // deployment storage posture for a joining user to consent to, so the
        // probe that existed only to gate it is gone too. (The NAT-mode seed the
        // probe also carried is irrelevant on this path: `nat_mode_choice` is
        // admin-claim-only, and both admin routes to it — handle-check and the
        // awaiting-manual-DNS recheck — seed `node_mode_seed` themselves.)

        let secret = match self.effective_secret() {
            Some(s) => s,
            None => {
                return {
                    use crate::snapshots::ErrorContext;
                    self.invite_error(ErrorContext::Redeeming, false, "no identity".into());
                    self.state.lock().unwrap().step
                };
            }
        };
        let signing_key = match parse_signing_key(&secret) {
            Some(k) => k,
            None => {
                return {
                    use crate::snapshots::ErrorContext;
                    self.invite_error(
                        ErrorContext::Redeeming,
                        false,
                        "invalid identity secret".into(),
                    );
                    self.state.lock().unwrap().step
                };
            }
        };
        let actor_id_bytes = signing_key.verifying_key().to_bytes();
        let actor_id_hex = hex::encode(actor_id_bytes);
        let timestamp_ms = Timestamp::now_millis();
        // Register the handle's BARE LOCAL PART, signed over `(local_part,
        // domain)` — the same split the claim path does (`handle_local_part` +
        // `parse_handle_domain`). Two nest-side rules force it, and both must be
        // satisfied at once (`account_core::register_core`):
        //
        //   1. `validate_handle` rejects '@' and runs BEFORE the signature check,
        //      so the whole wizard handle `alice@nest.example` is refused outright.
        //   2. The nest recomputes `domain` from its own configured handle_domain
        //      and verifies `actor_id || handle || domain || timestamp_be` against
        //      that domain plus its active mail domains — never against the empty
        //      string — so a bare handle signed over "" fails as signature_failed.
        //
        // This previously sent the whole handle and signed over the `@`-suffix,
        // tripping (1) on a `user@domain` handle and (2) on a bare one: redeeming
        // an invite could not succeed on any nest, on any client. Pinned by
        // `redeem_invite_registers_the_bare_local_part_signed_over_the_handle_domain`.
        let registered_handle = handle_local_part(&handle).to_string();
        let domain = parse_handle_domain(&handle).unwrap_or_default();
        let signed = fauna_protocol::account::register_signed_message(
            &actor_id_bytes,
            &registered_handle,
            &domain,
            timestamp_ms,
        );
        let signature_hex = sign_bytes(&secret, &signed);

        let body = crate::nest_api::RegisterBody {
            actor_id: actor_id_hex,
            handle: registered_handle,
            timestamp: timestamp_ms,
            signature: signature_hex,
            invite_code: oob_invite_code.clone(),
            age_claim: self.age_claim_to_send(),
        };
        match self.nest_api.register(&nest_url, body).await {
            Ok(_) => {
                // Per-app glue reads `wizard_outcome()` to drive next steps —
                // which is why the offer parks *without* setting it.
                // `Offer`: redeeming an out-of-band code is a JOIN, the
                // clearest instance of § 3b-ter's "joining user's first login".
                self.leave_for_logged_in(nest_url, handle, TrustOffer::Offer)
            }
            Err(crate::nest_api::RegisterError::Failed { cause: _ }) => {
                // Preserve the legacy cause string ("redeem failed") so the
                // i18n key the wizard surfaces to clients doesn't shift.
                use crate::snapshots::ErrorContext;
                self.invite_error(ErrorContext::Redeeming, true, "redeem failed".into());
                self.state.lock().unwrap().step
            }
            // The typed identity verdict (security.md § Pre-claim surfacing):
            // terminal — a retry re-signs a doomed handshake.
            Err(crate::nest_api::RegisterError::IdentityMismatch { reason }) => {
                use crate::snapshots::ErrorContext;
                self.invite_error(ErrorContext::Redeeming, false, reason);
                self.state.lock().unwrap().step
            }
        }
    }

    pub fn cancel_invite_op(&self) {
        self.cancel.raise();
        // Do not mutate snapshot — the in-flight task will short-circuit
        // and leave the snapshot at its last-known state, which the user
        // can then exit via Back.
    }

    /// Pre-seed a pending invite at app launch when the per-app long-term
    /// store has a record of one. Places the wizard at `InviteRequest` with
    /// `nest_url`/`handle`/the supplied snapshot loaded. Per-app glue calls
    /// this analogously to `seed_identity` after reading its store; the glue
    /// is responsible for the navigation that puts the user on the
    /// InviteRequest page.
    ///
    /// `status_json` is the JSON-serialized `InviteRequestState` enum from
    /// the per-app store. The wizard parses it back into a typed snapshot.
    /// If parsing fails (corrupt state, schema drift), the wizard falls back
    /// to `InviteRequestState::PendingReview` with the supplied request_id —
    /// the user can recheck and either resume or reset.
    /// Lands the wizard on `InviteRequest` with `nest_url` + `handle`
    /// pre-set and the snapshot reset to `Idle`. Per target
    /// `docs/goal/behavior/onboarding.md` §"App-launch routing": when the
    /// silent-challenge handshake reports the secret is unregistered on
    /// an otherwise-running nest, the user needs an invite — drop them
    /// directly on the invite-request page rather than make them
    /// re-type their handle. Pure state mutation; no IO.
    ///
    /// Distinct from `seed_pending_invite` (which restores a previously
    /// submitted request from the long-term store). Use this when there
    /// is no prior request to resume.
    pub fn navigate_to_invite_request_for_known_nest(&self, nest_url: String, handle: String) {
        use crate::snapshots::InviteRequestSnapshot;
        self.mutate(|s| {
            s.step = OnboardingStep::InviteRequest;
            s.nest_url = nest_url;
            s.handle = handle;
            s.error_message = None;
        });
        *self.invite_request.lock().unwrap() = InviteRequestSnapshot::idle();
    }

    /// Lands the wizard on `ClaimCode` with `nest_url` + `handle`
    /// pre-set and the snapshot reset to `Idle`. Per target
    /// `docs/goal/behavior/onboarding.md` § App-launch routing — silent-challenge
    /// fallback table (unclaimed-nest row): when /verify returns 404 AND
    /// `setup-status.claimed == false`, the saved nest is up but unclaimed,
    /// so the user must claim it themselves rather than ask for an invite.
    /// Pure state mutation; no IO.
    pub fn navigate_to_claim_code_for_known_nest(&self, nest_url: String, handle: String) {
        use crate::snapshots::ClaimCodeSnapshot;
        self.mutate(|s| {
            s.step = OnboardingStep::ClaimCode;
            s.nest_url = nest_url;
            s.handle = handle;
            s.error_message = None;
            // The ordinary unclaimed-nest path: the human types the admin's
            // printed code, so clear any stale prefill.
            s.claim_code_prefill = None;
        });
        *self.claim_code.lock().unwrap() = ClaimCodeSnapshot::idle();
    }

    /// Like [`navigate_to_claim_code_for_known_nest`], but also pre-loads a
    /// claim `code` the client already holds so the `ClaimCode` page can
    /// pre-fill its input (read via [`claim_code_prefill`]). Used by the
    /// factory-reset re-onboard affordance: `fauna.admin.factory_reset` returns
    /// the new claim code to the client and the human never sees it, so without
    /// pre-fill the admin would land on the claim-code page with nothing to
    /// type. After re-claim the nest is mode-unresolved, so the wizard still
    /// runs claim-code → encryption-mode (per
    /// `docs/goal/architecture/nest/storage-modes.md` § The claim-time choice).
    /// Pure state mutation; no IO.
    pub fn navigate_to_claim_code_for_known_nest_with_code(
        &self,
        nest_url: String,
        handle: String,
        code: String,
    ) {
        use crate::snapshots::ClaimCodeSnapshot;
        self.mutate(|s| {
            s.step = OnboardingStep::ClaimCode;
            s.nest_url = nest_url;
            s.handle = handle;
            s.error_message = None;
            s.claim_code_prefill = Some(code);
        });
        *self.claim_code.lock().unwrap() = ClaimCodeSnapshot::idle();
    }

    /// The claim code a client wants the `ClaimCode` page input pre-filled with
    /// (the factory-reset re-onboard path), or `None` for the ordinary path
    /// where the human types it. See
    /// [`navigate_to_claim_code_for_known_nest_with_code`].
    pub fn claim_code_prefill(&self) -> Option<String> {
        self.with_state(|s| s.claim_code_prefill.clone())
    }

    pub fn seed_pending_invite(
        &self,
        nest_url: String,
        handle: String,
        request_id: String,
        status_json: String,
    ) {
        use crate::snapshots::{InviteRequestSnapshot, InviteRequestState, OobCodeState};

        let state: InviteRequestState =
            serde_json::from_str(&status_json).unwrap_or(InviteRequestState::PendingReview {
                request_id: request_id.clone(),
                last_checked_ms: 0,
            });

        let recheck_visible = matches!(state, InviteRequestState::PendingReview { .. });
        // A hydrated slot carries no OOB code, so Continue starts dead — the
        // resumed journey advances by polling (§ The pending-invite surface).
        let continue_enabled = continue_enabled_for(&OobCodeState::Idle);

        self.mutate(|s| {
            s.step = OnboardingStep::InviteRequest;
            s.nest_url = nest_url.clone();
            s.handle = handle.clone();
        });
        *self.invite_request.lock().unwrap() = InviteRequestSnapshot {
            state,
            message: LocalizedText::default(),
            continue_enabled,
            recheck_visible,
            out_of_band_code_state: OobCodeState::Idle,
            oob_message: LocalizedText::default(),
            age_notice: None,
        };
    }

    /// The pending-invite resume slot for the current state, or `None` when
    /// there is nothing to resume.
    ///
    /// **This replaced `submit_invite_request_continue()` (retired 2026-08-12).**
    /// That method existed to *exit* the wizard with
    /// `WizardOutcome::InviteSubmitted`, and the exit is what `onboarding.md`
    /// § Wizard exit handling deletes: the pending-review journey never leaves
    /// the `invite_request` page — it stays there and polls until the poll
    /// resolves to `LoggedIn`. Apps call this at the
    /// [`Self::wizard_submit_invite_request`] return instead, which § 3
    /// Persistence callouts names as "the only write moment".
    ///
    /// Returning the assembled slot (rather than three getters) is deliberate:
    /// the two rules that are silent when wrong — `state.nest_url` over
    /// `effective_nest_url()`, and opaque `status_json` — live once here
    /// instead of being re-derived by seven apps. See [`PendingInviteSlot`].
    pub fn pending_invite_slot(&self) -> Option<crate::snapshots::PendingInviteSlot> {
        use crate::snapshots::InviteRequestState;
        let snap = self.invite_request.lock().unwrap().clone();
        let InviteRequestState::PendingReview { ref request_id, .. } = snap.state else {
            return None;
        };
        // state.nest_url, NOT effective_nest_url(): this URL is persisted as the
        // nest's identity. The provider_base_urls override retargets HTTP
        // requests only — leaking it here would put a test-cloud URL into a
        // production identity-store record.
        let nest_url = self.state.lock().unwrap().nest_url.clone();
        Some(crate::snapshots::PendingInviteSlot {
            nest_url,
            handle: self.current_handle(),
            request_id: request_id.clone(),
            // Opaque to the app (`onboarding.md` § Long-term store contract:
            // "Don't validate the JSON"). Degrade to empty rather than strand
            // the request — the wizard falls back to `PendingReview` on reseed.
            status_json: serde_json::to_string(&snap.state).unwrap_or_default(),
        })
    }

    fn invite_error(
        &self,
        ctx: crate::snapshots::ErrorContext,
        transient: bool,
        cause: String,
    ) -> OnboardingStep {
        use crate::snapshots::InviteRequestState;
        self.invite_request.lock().unwrap().state = InviteRequestState::Error {
            transient,
            context: ctx,
            cause,
        };
        self.observer.on_changed();
        self.state.lock().unwrap().step
    }
}

// ── Claim-code stage ──────────────────────────────────────────────────────

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl OnboardingMachine {
    /// Wire-up for the `claim-code-submit-button`. POSTs the user's
    /// one-time claim code + Ed25519-signed auth payload to
    /// `POST /api/v1/claim-admin`. On 2xx, the wizard transitions to
    /// `Done` with `wizard_outcome() == LoggedIn { nest_url, handle }`;
    /// on 4xx the wizard stays on `claim_code` with the snapshot moved
    /// to `Invalid { reason }`; on 5xx / network the snapshot moves to
    /// `Error { transient: true, ... }`. Per
    /// `docs/goal/behavior/onboarding.md` §3a.
    pub async fn wizard_submit_claim_code(&self, code: String) -> OnboardingStep {
        use crate::nest_api::ClaimAdminError;
        use crate::snapshots::ClaimCodeState;

        // Move snapshot to Submitting and fire an observer tick.
        {
            let mut snap = self.claim_code.lock().unwrap();
            snap.state = ClaimCodeState::Submitting;
            snap.message = LocalizedText {
                key: "onboarding.claim_code.submitting".into(),
                args: Default::default(),
            };
            snap.submit_enabled = false;
        }
        self.observer.on_changed();

        // The input may be the console-printed `fauna://claim` URI rather than
        // the bare code. Parse first, and pin BEFORE anything else: when the
        // URI carries the nest's identity, hold it as the first-contact root
        // for the claim host so the very connection the code is sent over is
        // verified against the identity the console vouched for
        // (security.md § Transport trust, Axis 2 — the self-hosted
        // public-domain shape, which otherwise has no root at claim time;).
        // A malformed URI, or a pinned URI whose target
        // host cannot be determined, refuses loudly — an input that looks
        // protected must never silently continue unpinned.
        let parsed = match fauna_core::claim_code::parse_claim_input(&code) {
            Some(p) => p,
            None => {
                return self.claim_code_invalid(
                    "unrecognized claim input — paste the code, or the full \
                     fauna://claim URI from the nest console",
                );
            }
        };
        let code = parsed.code;
        if let Some(nest_hex) = parsed.nest_actor_id.as_deref() {
            let expected = match hex::decode(nest_hex)
                .ok()
                .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            {
                Some(e) => e,
                // Unreachable in practice — the parser admits only 64-hex —
                // kept as a loud refusal rather than a silent unpin.
                None => {
                    return self
                        .claim_code_invalid("the claim URI carries an unreadable nest identity");
                }
            };
            match crate::helpers::nest_host(&self.effective_nest_url()) {
                Some(host) => self.hold_first_contact_identity(&host, expected),
                None => {
                    return self.claim_code_invalid(
                        "cannot pin the nest identity: no nest address is set",
                    );
                }
            }
        }

        let secret = match self.effective_secret() {
            Some(s) => s,
            None => {
                return self.claim_code_error(
                    false,
                    "no identity".into(),
                    "onboarding.claim_code.error.terminal",
                );
            }
        };
        let nest_url = self.effective_nest_url();
        // Register the wizard's chosen handle in the claim itself, so claiming
        // an unclaimed nest with a handle makes that handle the admin's address
        // (the canonical-alias-on-enable behavior depends on the handle being
        // set). The nest stores the bare local-part (its handle validation
        // rejects '@'); a wizard handle like `alice@nest.example` carries the
        // domain for nest discovery AND as the mail domain, so register the bare
        // local part as the handle and send the domain as `mail_domain` for the
        // nest to auto-register (the handle becomes a routable email with no
        // manual admin-dns add-domain step). An empty handle is rejected below
        // (a handle is required to claim).
        let wizard_handle = self.with_state(|s| s.handle.trim().to_string());
        let handle = handle_local_part(&wizard_handle).to_string();
        // A handle is REQUIRED to claim (a handle-less admin is unrepresentable —
        // mail/AUTH login resolves nobody). Guard here so the client surfaces the
        // same clear `Invalid` error the nest would return, without a pointless
        // round-trip. An empty handle at this point means the client failed to
        // thread one in (e.g. the factory-reset re-onboard must source the
        // admin's authoritative handle).
        if handle.is_empty() {
            return self.claim_code_invalid("a handle is required to claim the nest");
        }
        let mail_domain = parse_handle_domain(&wizard_handle);
        let mail_domain_opt = mail_domain.as_deref();

        match self
            .nest_api
            .claim_admin(&nest_url, &code, &secret, &handle, mail_domain_opt)
            .await
        {
            Ok(_resp) => {
                // `resp.deployment_seed` is deliberately not consumed: the
                // launched client's custody leg fetches the seed from the box
                // itself (box-recovery.md § The plane-era recovery floor, (c)).
                // The claim axis of the § 3b derivation (`state.claim_completed`):
                // this run has now claimed the box, so the four serving-enablement
                // intents may be requested. Set here rather than read off the
                // claim-code snapshot below, because the manual-DNS claim path
                // (`complete_manual_dns_claim`) never touches that snapshot's state.
                self.state.lock().unwrap().claim_completed = true;
                // A pasted `fauna://claim` URI held the console-vouched root and
                // this claim's connection was graduated against it — persist the
                // proof as the domain's durable pin (`security.md` § Transport
                // trust). A bare-code claim held nothing and seeds nothing.
                self.seed_identity_pin_at_claim(&nest_url);
                {
                    let mut snap = self.claim_code.lock().unwrap();
                    snap.state = ClaimCodeState::Claimed;
                    snap.message = LocalizedText {
                        key: "onboarding.claim_code.claimed".into(),
                        args: Default::default(),
                    };
                    snap.submit_enabled = false;
                }
                // Claim completion is the anchor for the wizard's single,
                // terminal setup step (`storage-modes.md` § What replaced each
                // piece of the axis — the claim-time trust question is gone; a
                // nest is sealed and content-ready from first boot).
                // `wizard_outcome` is NOT set here — it is set by
                // `submit_nat_mode_choice` / `defer_nat_mode_choice`. Reset the
                // NAT snapshot to its seeded default in case a prior aborted
                // attempt left it in an error state.
                self.reset_nat_mode_snapshot();
                self.state.lock().unwrap().step = OnboardingStep::NatModeChoice;
                self.observer.on_changed();
                OnboardingStep::NatModeChoice
            }
            Err(ClaimAdminError::Invalid { reason }) => self.claim_code_invalid(reason),
            Err(ClaimAdminError::Transient { cause }) => {
                self.claim_code_error(true, cause, "onboarding.claim_code.error.transient")
            }
            Err(ClaimAdminError::InvalidIdentity { cause }) => {
                self.claim_code_error(false, cause, "onboarding.claim_code.error.terminal")
            }
            // The typed identity verdict (security.md § Pre-claim surfacing):
            // the box did not prove the identity the pasted `fauna://claim`
            // URI (or the injected seed) requires. Terminal, no retry CTA —
            // a resubmit re-earns the verdict.
            Err(ClaimAdminError::IdentityMismatch { reason }) => {
                self.claim_code_error(false, reason, "onboarding.claim_code.error.terminal")
            }
            // The nest can't read its own claim code — a server-side
            // misprovisioning, not a wrong code. Terminal (retryable=false), with
            // its own dedicated message distinct from already_claimed/invalid.
            Err(ClaimAdminError::Misprovisioned { cause }) => self.claim_code_error(
                false,
                cause,
                "onboarding.claim_code.error.claim_code_unreadable",
            ),
            // The code was fine — the nest already has an admin (the
            // within-grace pending-factory-reset honor arm's failed-dispatch
            // exit reaches here: a fresh slot pre-fills this page against a box
            // whose reset never actually landed). Own dedicated message.
            Err(ClaimAdminError::AlreadyClaimed) => self.claim_code_already_claimed(),
        }
    }

    /// Polls the freshly-provisioned nest from the post-provisioning
    /// "Almost ready" surface. The client calls this on a timer while
    /// `wizard_outcome()` is `AwaitingManualDns` (the deferred-DNS exit),
    /// single-shot — exactly like `recheck_invite_status`: one
    /// `probe_setup_status` reachability+claim-status probe over WS-RPC, and
    /// if the nest is reachable and unclaimed, one `claim_admin` call.
    ///
    /// Every call here is **pre-claim**: it rides the anonymous WS-RPC
    /// connection — there is no authenticated actor session until the claim
    /// succeeds. (A `setup-status` failure means DNS hasn't propagated yet or
    /// the nest is still booting; both surface to the user as "waiting for
    /// your nest to come online".) On a successful claim the wizard routes to
    /// `NatModeChoice` — the same path `wizard_submit_claim_code` takes —
    /// clearing the awaiting outcome. On the already-claimed resume it skips
    /// that setup step and concludes through § 3b-ter's offer instead
    /// (`TrustPrompt` on an app that renders it, else straight to `Done`).
    ///
    /// No-op (returns the current step) unless `wizard_outcome()` is
    /// `AwaitingManualDns`. Transient failures (nest unreachable, 5xx on
    /// claim) leave the snapshot `Pending` so the client keeps polling; a
    /// rejected claim, a corrupt identity, or a first-contact identity
    /// mismatch (security.md § Pre-claim surfacing) yields a terminal
    /// `Error`. Per `docs/goal/behavior/onboarding.md` § "Wizard exit
    /// handling".
    pub async fn recheck_manual_dns(&self) -> OnboardingStep {
        let claim_code = match self.wizard_outcome() {
            Some(WizardOutcome::AwaitingManualDns { claim_code, .. }) => claim_code,
            _ => return self.with_state(|s| s.step),
        };

        let nest_url = self.effective_nest_url();
        // The surface's rendering of the shared core's two phases.
        let outcome = self
            .claim_provisioned_box(&nest_url, &claim_code, |phase| match phase {
                ClaimPhase::Probing => self.set_awaiting_dns_state(
                    AwaitingDnsState::Checking,
                    "onboarding.awaiting_dns.checking",
                ),
                ClaimPhase::Claiming => self.set_awaiting_dns_state(
                    AwaitingDnsState::Claiming,
                    "onboarding.awaiting_dns.claiming",
                ),
            })
            .await;

        match outcome {
            ClaimOutcome::Claimed { already_claimed } => {
                self.complete_manual_dns_claim(already_claimed)
            }
            // Not reachable yet, or a transient claim failure: back to Pending
            // so the client's timer keeps polling. Never an Error — writing one
            // here would wedge the poll behind a terminal screen.
            ClaimOutcome::NotYet => {
                // Read the records into a local FIRST: a `lock()` inside the
                // argument list would still be held when `set_awaiting_dns_state`
                // locks the same (non-reentrant) mutex, deadlocking the poll.
                let records = self.awaiting_manual_dns.lock().unwrap().dns_records.clone();
                self.set_awaiting_dns_state(
                    AwaitingDnsState::Pending,
                    crate::snapshots::awaiting_manual_dns::resting_message_key(&records),
                );
                self.with_state(|s| s.step)
            }
            ClaimOutcome::Failed { cause } => self.awaiting_dns_error(cause),
        }
    }
}

/// Which link of [`OnboardingMachine::claim_provisioned_box`]'s chain is running,
/// so each caller renders it in its own idiom — the "Almost ready" surface as an
/// `AwaitingDnsState`, the provisioning run as an `Online` substep.
pub(crate) enum ClaimPhase {
    /// Dialling `fauna.setup.status` at the reach address.
    Probing,
    /// The box answered and is unclaimed; `fauna.auth.claim_admin` is in flight.
    Claiming,
}

/// What one pass of [`OnboardingMachine::claim_provisioned_box`] concluded.
pub(crate) enum ClaimOutcome {
    /// The box is ours and claimed. `already_claimed` is the recovery edge — our
    /// claim landed and its reply was lost — confirmed by the silent challenge,
    /// never by a second claim.
    Claimed { already_claimed: bool },
    /// Nothing is wrong yet: the box is not reachable, or the claim hit a
    /// transient failure. The caller polls again; it must NOT surface an error.
    NotYet,
    /// Terminal — the caller surfaces `cause` and stops.
    Failed { cause: String },
}

impl OnboardingMachine {
    /// The provisioning run's own leg of [`Self::claim_provisioned_box`] — the
    /// `Online` claiming substep (`docs/goal/behavior/onboarding.md` § 6
    /// *Provisioning = build + claim*).
    ///
    /// Dials the **reach address**: the box answers on its captured public IP the
    /// moment cloud-init finishes and on its domain only much later, so the
    /// resolve override is installed from the run's result before the claim rather
    /// than by `stash_provisioning_result` after it (§ 6 *Reaching the box*).
    ///
    /// A refused claim fails `Online` with the claim's own error, which is the
    /// state the page's Retry button resumes from: a retry re-runs the whole
    /// orchestrator, whose idempotency skips the built steps, re-polls, and
    /// re-claims. A transient failure lands in the same place deliberately — the
    /// orchestrator's `Online` step already proved `/health` answers, so there is
    /// nothing left for this call to wait on, and Retry is the user's resume.
    ///
    /// `nest_url` is the caller's own just-computed provisioning-path URL
    /// (`NextStep::Provisioned { url }`), NOT `effective_nest_url()` /
    /// `state.nest_url` — that field is populated later, by
    /// `stash_provisioning_result` after this whole function returns, so
    /// reading it here always found "" and every claim's first probe failed
    /// on an unparseable empty URL before ever reaching the box (measured
    /// 2026-08-29: `WebSocket error: invalid ws url:
    /// URL error: No host name in the URL nest_url=""`).
    async fn run_provisioning_claim(self: &Arc<Self>, nest_url: &str, claim_code: &str) {
        use fauna_provisioning::error::ProvisionError;
        use fauna_provisioning::progress::{self, ProvisionStep, SubstepKey};

        self.install_reach_override();
        progress::set_claiming(&self.provisioning);
        self.observer.on_changed();

        let outcome = self
            .claim_provisioned_box(nest_url, claim_code, |phase| {
                let key = match phase {
                    ClaimPhase::Probing => SubstepKey::OnlineWaiting,
                    ClaimPhase::Claiming => SubstepKey::OnlineClaiming,
                };
                progress::set_substep(&self.provisioning, ProvisionStep::Online, key);
                self.observer.on_changed();
            })
            .await;

        match outcome {
            ClaimOutcome::Claimed { .. } => {
                // The claim axis of the § 3b derivation, exactly as the surface's
                // claim sets it — the launch glue's claim-time enablement is owed
                // either way.
                self.mutate(|s| s.claim_completed = true);
                self.forget_pending_provision_row();
                progress::set_claim_succeeded(&self.provisioning);
            }
            ClaimOutcome::NotYet => progress::set_failed(
                &self.provisioning,
                ProvisionStep::Online,
                1,
                &ProvisionError::Other("the nest did not complete the claim".into()),
            ),
            ClaimOutcome::Failed { cause } => progress::set_failed(
                &self.provisioning,
                ProvisionStep::Online,
                1,
                &ProvisionError::Other(cause),
            ),
        }
        self.observer.on_changed();
    }

    /// The orchestrator's `on_server_ready` hook: publish the box's reach
    /// address and complete the pending-provision slot with it, the moment
    /// the Server step settles (`docs/goal/behavior/onboarding.md` § 6 *Reaching
    /// the box* / *The pending-provision slot*).
    ///
    /// This is the only moment the address is knowable mid-run — nothing reaches
    /// the shared `ProvisioningSnapshot` until `set_run_succeeded`, i.e. after
    /// `Online`, and the crash window the slot exists for is precisely *before*
    /// that.
    ///
    /// It is also the moment the box's identity becomes a fact rather than a
    /// plan. `origin` says which: a box **created** now boots with this run's
    /// `fresh_identity` (the seed just baked into its cloud-init), so that
    /// identity is held as the first-contact root from here on and lands in
    /// the slot — which is what corrects a retry that had to re-create the box
    /// after holding the retained identity provisionally. A box **found**
    /// already there boots with whatever an earlier run injected: the identity
    /// this machine retained BEFORE this run (`prior_identity` — already held)
    /// if an earlier run of this client built it, and nothing knowable
    /// otherwise — that box is not ours to expect anything of, and the
    /// provisional fresh root stays held so its first contact hard-fails rather
    /// than TOFU-claiming a stranger's box with a code it does not have. The
    /// row this run retained before the orchestrator ran is deliberately NOT
    /// consulted here: it carries this run's *planned* identity, which a box
    /// that already existed cannot have.
    ///
    /// A failed completion is logged and swallowed, unlike the mint's: losing the
    /// address costs a resumed run its head start (the surface falls back to
    /// waiting for DNS), while losing the *code* would orphan the box. Different
    /// stakes, different answers.
    #[allow(clippy::too_many_arguments)]
    fn note_server_ready(
        &self,
        ipv4: &str,
        origin: ServerOrigin,
        domain: &str,
        nest_url: &str,
        handle: &str,
        claim_code: &str,
        fresh_identity: Option<&str>,
        prior_identity: Option<&str>,
    ) {
        *self
            .provision_reach_ipv4
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(ipv4.to_string());
        let boots_with: Option<String> = match origin {
            ServerOrigin::Created => {
                if let Some(id) = fresh_identity.and_then(decode_nest_actor_id_hex) {
                    self.hold_first_contact_identity(domain, id);
                }
                fresh_identity.map(str::to_string)
            }
            ServerOrigin::Found => {
                // The retained row is the first answer, and the only one on the
                // paths that have not claimed yet. But a box this client
                // ALREADY CLAIMED has no retained row by design
                // (`forget_pending_provision_row` — custody is discharged at
                // the claim), and that is precisely the box a Retry pressed on
                // a *Succeeded* page finds. Falling straight through to the
                // fresh seed there held a root the box cannot match and
                // hard-failed the client against its own nest —
                // `nest_actor_id is not the expected nest`, the right refusal
                // aimed at the wrong box.
                //
                // Two memories answer it, strongest first: what the claim
                // itself held (`claimed_box_identity`, this session), then the
                // durable pin that claim persisted (`seed_identity_pin_at_claim`),
                // which is what a relaunched app has instead.
                let retained = prior_identity
                    .map(str::to_string)
                    .or_else(|| self.claimed_identity_at(domain).map(hex::encode))
                    .or_else(|| self.claimed_identity_pin(domain).map(hex::encode));
                if retained.is_none() {
                    tracing::warn!(
                        target: "fauna_onboarding",
                        domain,
                        ipv4,
                        "the Server step found a box of this name that this client did not \
                         build (or whose identity it no longer holds); its first contact will \
                         not verify — delete it at the provider, or resume the pending claim"
                    );
                } else if prior_identity.is_none() {
                    tracing::info!(
                        target: "fauna_onboarding",
                        domain,
                        ipv4,
                        "the Server step found a box this client has already CLAIMED; \
                         expecting the identity that claim pinned, not this run's fresh seed"
                    );
                }
                // Hold it: unlike the pre-orchestrator hold, which had only the
                // retained row to go on, this arm may have just learned the
                // identity from the pin — and nothing downstream re-holds on the
                // found path.
                if let Some(id) = retained.as_deref().and_then(decode_nest_actor_id_hex) {
                    self.hold_first_contact_identity(domain, id);
                }
                retained
            }
        };
        self.retain_pending_provision(AwaitingDnsRecord {
            nest_url: nest_url.to_string(),
            handle: handle.to_string(),
            dns_records_json: String::new(),
            claim_code: claim_code.to_string(),
            reach_ipv4: Some(ipv4.to_string()),
            nest_actor_id: boots_with.clone(),
        });
        let store = self
            .pending_provision
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(store), Some(secret)) = (store, self.effective_secret()) else {
            return;
        };
        if !fauna_launch_machine::complete_pending_provision_reach(
            store.as_ref(),
            secret,
            nest_url.to_string(),
            handle.to_string(),
            claim_code.to_string(),
            boots_with,
            ipv4.to_string(),
        ) {
            tracing::warn!(
                "[onboarding] the pending-provision slot did not take the reach address; \
                 a resumed run will have to wait for DNS"
            );
        }
    }

    /// The pending-provision row this machine holds — the box it is mid-way
    /// through provisioning, as last written to the slot (`§ 6 *The
    /// pending-provision slot*`); `None` once the box is claimed or before any
    /// run. Rust-only (tests and app glue); the apps' own copy is the slot.
    pub fn pending_provision_row(&self) -> Option<AwaitingDnsRecord> {
        self.pending_provision_row
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The first-contact identity root currently held, as `(host, actor id
    /// hex)` — the injected-seed root of a box this client provisioned, or the
    /// console-pasted identity of a self-hosted claim (`security.md`
    /// § Transport trust, Axis 2). Rust-only, for tests: the machine consults
    /// it through `WsNestApi`, never through this getter.
    pub fn first_contact_identity_held(&self) -> Option<(String, String)> {
        self.nest_expected_identity
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .map(|(host, id)| (host, hex::encode(id)))
    }

    /// Persist the held, possession-proven first-contact root as the domain's
    /// durable nest-identity pin — called at every claim success, so the proof
    /// the wizard graduated its pre-claim dials against outlives the wizard
    /// instead of being discarded at exit (`security.md` § Transport trust: the
    /// first post-claim dial — usually over the reach hint, before the domain
    /// resolves — then **verifies** against this pin instead of TOFU-ing
    /// whatever answers at the stored address).
    ///
    /// No root held (a bare-code claim, a slot written with `nest_actor_id` None)
    /// → writes nothing: nothing was proven, so the launch path stays TOFU
    /// exactly as today. A held root targeting a different host than
    /// `nest_url` writes nothing either — the same host-match guard
    /// `expected_root_for` applies, so the root is only ever asserted against
    /// the box it was provisioned for, never a bystander domain.
    ///
    /// Each arm writes the key its launch reader resolves: native the URL's
    /// authority (`seed_claimed_identity_pin` → `authority_of`, the key
    /// `connect_silent_challenge` graduates against), wasm the nest URL
    /// verbatim (the key the machine's connector hands
    /// `run_pinned_silent_challenge`). The write is deliberately authoritative
    /// — a "start over onto a new box, same domain" replaces the prior box's
    /// pin rather than wedging the first launch on `IdentityChanged` — see
    /// `seed_claimed_identity_pin`'s doc for the full rationale.
    /// The durable identity pin a previous **claim** of this `domain` left
    /// behind, if any — the read half of [`Self::seed_identity_pin_at_claim`],
    /// through the same installed store and the same key, so what one wrote the
    /// other finds.
    ///
    /// Keyed off the **identity URL** (`https://{domain}`), never off a slot or
    /// provider-override address: the pin is written from the claim, which dials
    /// the identity URL, and `authority_of` keeps the port out of a default-port
    /// https authority. A test rig (or the deferred path) whose provider
    /// override points the transport at `http://127.0.0.1:PORT` still finds the
    /// pin it wrote, because both halves name the domain.
    ///
    /// This is a *proven* identity, not a planned one: it is written only after
    /// a graduated handshake, which is why it outranks this run's fresh seed as
    /// the expected root for a box the Server step merely **found**.
    fn claimed_identity_pin(&self, domain: &str) -> Option<[u8; 32]> {
        let identity_url = format!("https://{domain}");
        #[cfg(not(target_arch = "wasm32"))]
        {
            fauna_anon_client::trust::pinned_identity(&fauna_anon_client::trust::authority_of(
                &identity_url,
            ))
        }
        #[cfg(target_arch = "wasm32")]
        {
            use fauna_client_core::nest_trust::NestIdentityPinStore as _;
            fauna_client_core::nest_trust::LocalStoragePinStore.get(&identity_url)
        }
    }

    fn seed_identity_pin_at_claim(&self, nest_url: &str) {
        let Some((host, id)) = self
            .nest_expected_identity
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        if crate::helpers::nest_host(nest_url).as_deref() != Some(host.as_str()) {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        fauna_anon_client::trust::seed_claimed_identity_pin(nest_url, id);
        #[cfg(target_arch = "wasm32")]
        {
            use fauna_client_core::nest_trust::NestIdentityPinStore as _;
            let store = fauna_client_core::nest_trust::LocalStoragePinStore;
            // Same guard as the native seam: re-seeding the root the pin
            // already names would erase a chain-accepted rotation seq (the
            // `@seq` suffix `set` never writes), disarming fork detection; a differing root still
            // overwrites authoritatively.
            if store.get(nest_url) != Some(id) {
                store.set(nest_url, id);
            }
        }
    }

    /// The pending-provision row this machine holds for `(nest_url, handle)`,
    /// if it is mid-way through provisioning exactly that box — the key a
    /// re-run of the same domain by the same identity matches on.
    fn retained_pending_provision(
        &self,
        nest_url: &str,
        handle: &str,
    ) -> Option<AwaitingDnsRecord> {
        self.pending_provision_row
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .filter(|row| row.nest_url == nest_url && row.handle == handle)
    }

    fn retain_pending_provision(&self, row: AwaitingDnsRecord) {
        *self
            .pending_provision_row
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(row);
    }

    /// The box is claimed — ours, verified — so there is nothing left to
    /// resume: a later provisioning of the same domain is a NEW box and mints
    /// its own code and identity.
    fn forget_pending_provision_row(&self) {
        // Keep the one fact the row carried that outlives custody: WHICH box is
        // at this domain. The held first-contact root is that identity right now
        // (the claim just graduated against it), and it is about to be
        // overwritten by the next run's fresh seed — so snapshot it here, where
        // it is still true. See `claimed_box_identity`.
        let claimed = self
            .nest_expected_identity
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(pair) = claimed {
            *self
                .claimed_box_identity
                .write()
                .unwrap_or_else(|e| e.into_inner()) = Some(pair);
        }
        *self
            .pending_provision_row
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The identity of a box this machine already claimed at `domain`, if that
    /// is the domain it claimed. See [`Self::claimed_box_identity`].
    fn claimed_identity_at(&self, domain: &str) -> Option<[u8; 32]> {
        self.claimed_box_identity
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(host, _)| host == domain)
            .map(|(_, id)| *id)
    }

    /// Point pre-identity calls for this box's domain at its captured public IP —
    /// the reach address every link of the post-build chain dials
    /// (`docs/goal/behavior/onboarding.md` § 6 *Reaching the box*), while
    /// `state.nest_url` stays `https://{domain}` as the identity URL.
    ///
    /// Idempotent, and called from both ends of the run: before the claim (which
    /// needs it) and again from `stash_provisioning_result` (which is also the
    /// only caller on the paths that do not claim). The **resume**'s arming is
    /// [`Self::seed_awaiting_manual_dns_record`]'s; both go through
    /// [`Self::hold_reach_override`].
    fn install_reach_override(&self) {
        if let Some(r) = self.provisioning.lock().unwrap().result.clone() {
            self.hold_reach_override(&r.domain, &r.ipv4);
        }
    }

    /// Arm the reach address for `host` — the box is up at `ipv4` now, and by
    /// its domain only much later (`onboarding.md` § 6 *Reaching the box*).
    ///
    /// One writer, two arming moments: the run's own (`install_reach_override`,
    /// from the Server step's result) and the **resume**'s
    /// ([`Self::seed_awaiting_manual_dns_record`], from the pending-provision
    /// slot's `reach_ipv4`) — which is what makes "the poll dials the box at the
    /// slot's `reach_ipv4`" (§ "Almost ready" surface, *Reach*) true after a
    /// relaunch and not only inside the run that captured the address.
    ///
    /// The pair is `(identity host, address)` and both dial mechanisms key on
    /// the host: native resolves that name to this address at the socket
    /// (SNI/`Host`/cert identity all stay the name), and wasm — which can
    /// override neither DNS nor TLS — dials `https://{ip}` literally while still
    /// asserting the first-contact root against the *name*
    /// (`nest_api::ws_nest_api`). A malformed address arms nothing, leaving the
    /// dial on system DNS, which is the plain-DNS dial.
    fn hold_reach_override(&self, host: &str, ipv4: &str) {
        if let Ok(ip) = ipv4.parse() {
            *self
                .nest_resolve_override
                .write()
                .unwrap_or_else(|e| e.into_inner()) = Some((host.to_string(), ip));
        }
    }

    /// **The one claim implementation for a box this client provisioned**:
    /// probe → claim → capture the deployment seed, with
    /// no routing and no UI of its own.
    ///
    /// Two callers share it, which is the point (`docs/goal/behavior/onboarding.md`
    /// § 6 *Provisioning = build + claim*: "the same core the 'Almost ready'
    /// surface's `recheck_manual_dns()` runs — one claim implementation, shared by
    /// the in-session finish and the relaunch resume"): the provisioning run's
    /// `Online` claiming substep, and the surface's poll after a quit or crash.
    /// Everything here is **pre-claim** — it rides the anonymous WS-RPC
    /// connection, since there is no authenticated actor session until the claim
    /// succeeds.
    ///
    /// The already-claimed arm is the recovery edge, and it **confirms the box is
    /// ours by the silent challenge** rather than assuming it: `claimed: true`
    /// alone cannot tell our own lost-reply claim from someone else's box, and
    /// treating the second as the first would sign the user in to a nest they do
    /// not own. A challenge that cannot answer (unreachable, degraded) is
    /// [`ClaimOutcome::NotYet`], not an error — the next poll asks again.
    pub(crate) async fn claim_provisioned_box(
        &self,
        nest_url: &str,
        claim_code: &str,
        on_phase: impl Fn(ClaimPhase) + Sync,
    ) -> ClaimOutcome {
        use crate::nest_api::{ClaimAdminError, ProbeError, SilentChallengeOutcome};

        on_phase(ClaimPhase::Probing);
        let status = match self.nest_api.probe_setup_status(nest_url).await {
            Ok(s) => s,
            // The typed identity verdict (security.md § Pre-claim surfacing):
            // the probe's fresh connection failed first-contact graduation
            // against the held root — the server at the reach address is not
            // the box this client provisioned. Terminal, never `NotYet`:
            // flattening it into the poll rendered a MITM signal as an
            // endless "waiting for DNS" spinner, and the benign twin (a box that booted with an
            // identity other than the injected seed) deserves the same stop.
            Err(ProbeError::IdentityMismatch { reason }) => {
                tracing::warn!(
                    target: "fauna_onboarding",
                    error = %reason,
                    nest_url,
                    "claim_provisioned_box: first-contact identity mismatch — terminal"
                );
                return ClaimOutcome::Failed { cause: reason };
            }
            // Not reachable yet (still booting, or — on the surface's path — DNS
            // has not propagated). Keep polling.
            //
            // Logged, not silent: this arm used to swallow the real error, so
            // every caller — app UI, e2e harness, a human reading a stuck
            // Online step — only ever saw the generic "the nest did not
            // complete the claim" with no way to tell "box still booting"
            // from a genuine bug. That silence cost 5 live e2e runs on ios
            // before diagnostic logging here caught an empty `nest_url` bug — worth keeping
            // permanently at `warn`, not stripping back out once the one bug
            // it caught is fixed.
            Err(e) => {
                tracing::warn!(
                    target: "fauna_onboarding",
                    error = %e,
                    nest_url,
                    "claim_provisioned_box: probe_setup_status failed, reporting NotYet"
                );
                return ClaimOutcome::NotYet;
            }
        };

        // Capture the resolved NAT-mode seed so the downstream `nat_mode_choice`
        // entry pre-selects it — the same capture the handle-check probe does.
        self.mutate(|s| s.node_mode_seed = status.node_mode);

        let Some(secret) = self.effective_secret() else {
            return ClaimOutcome::Failed {
                cause: "no identity".into(),
            };
        };

        if status.claimed {
            return match self.nest_api.silent_challenge(nest_url, &secret).await {
                // The box knows this identity, so the claim it holds is ours:
                // our claim landed and only its reply was lost.
                SilentChallengeOutcome::Success(_) => {
                    // A claim success like any other: the challenge's fresh
                    // connection was graduated against the held root, so
                    // persist the proof as the domain's durable pin.
                    self.seed_identity_pin_at_claim(nest_url);
                    ClaimOutcome::Claimed {
                        already_claimed: true,
                    }
                }
                // The box is claimed and does not know us — someone else's nest.
                // Terminal: there is nothing to poll for.
                SilentChallengeOutcome::NotRegistered => ClaimOutcome::Failed {
                    cause: "already_claimed".into(),
                },
                // A reachability fault (server error, timeout, malformed
                // reply, disconnect, connect-refused, expired/consumed
                // nonce) — retryable, a retry gets a fresh nonce.
                SilentChallengeOutcome::Transient { error: _ } => ClaimOutcome::NotYet,
                // The nest booted a degraded "needs-update" mode
                // (fauna.nest.outdated) — terminal; the fix is updating the
                // nest, not a retry (version-compatibility.md Dim 4).
                SilentChallengeOutcome::NeedsUpdate { message: _ } => ClaimOutcome::Failed {
                    cause: "nest outdated".into(),
                },
                // The secret isn't a valid 32-byte Ed25519 key. Terminal.
                SilentChallengeOutcome::SecretInvalid { error: _ } => ClaimOutcome::Failed {
                    cause: "signature mismatch".into(),
                },
                // This identity was succeeded (identity-succession.md
                // § Propagation → Own device fleet). Terminal, and terminal
                // for a reason no retry touches: the old key still produces
                // valid signatures forever. The import affordance belongs to
                // the LAUNCH surface, not this pre-identity probe.
                SilentChallengeOutcome::Superseded { .. } => ClaimOutcome::Failed {
                    cause: "identity superseded".into(),
                },
                // A locked account: terminal until `locked_until`, and the
                // locked surface is the LAUNCH path's, not this probe's.
                SilentChallengeOutcome::Locked { .. } => ClaimOutcome::Failed {
                    cause: "account locked".into(),
                },
                // The typed identity verdict (security.md § Pre-claim
                // surfacing): a pin IS held on this path — the provisioning
                // run holds the injected seed's identity as the first-contact
                // root for this host (`hold_first_contact_identity`), and
                // `WsNestApi::core` graduates the challenge's fresh connection
                // against it, mapping a held-root or pin graduation failure to
                // this variant. Terminal: never a silent re-pin, never a retry
                // loop — and NO re-trust affordance anywhere in the wizard
                // (with a held root the remedy is re-provisioning; the launch
                // surface's re-trust is for TOFU pins, not injected roots).
                SilentChallengeOutcome::IdentityChanged { .. } => ClaimOutcome::Failed {
                    cause: "nest identity changed".into(),
                },
            };
        }

        // Reachable and unclaimed → claim. Carry the chosen handle's local part
        // as the handle and its `@domain` suffix as `mail_domain` for the nest to
        // auto-register (the handle becomes a routable email, no manual
        // add-domain) — the same shape `wizard_submit_claim_code` sends.
        on_phase(ClaimPhase::Claiming);
        let wizard_handle = self.with_state(|s| s.handle.trim().to_string());
        let handle = handle_local_part(&wizard_handle).to_string();
        // A handle is required (see `wizard_submit_claim_code`). It was chosen on
        // `handle_entry` on both paths that reach here, so it should never be
        // empty; guard defensively rather than round-trip a claim the nest will
        // reject.
        if handle.is_empty() {
            return ClaimOutcome::Failed {
                cause: "a handle is required to claim the nest".into(),
            };
        }
        let mail_domain = parse_handle_domain(&wizard_handle);
        match self
            .nest_api
            .claim_admin(
                nest_url,
                claim_code,
                &secret,
                &handle,
                mail_domain.as_deref(),
            )
            .await
        {
            Ok(_resp) => {
                // `resp.deployment_seed` is not consumed — the custody leg
                // fetches it (box-recovery.md § The plane-era recovery floor, (c)).
                // The claim rode a connection graduated against the held
                // first-contact root — persist the proof as the domain's
                // durable pin so the first post-claim dial verifies instead
                // of TOFU-ing (`security.md` § Transport trust).
                self.seed_identity_pin_at_claim(nest_url);
                ClaimOutcome::Claimed {
                    already_claimed: false,
                }
            }
            // The box is up but the claim hit a transient failure; poll again
            // rather than surfacing a terminal error.
            Err(ClaimAdminError::Transient { .. }) => ClaimOutcome::NotYet,
            Err(ClaimAdminError::Invalid { reason }) => ClaimOutcome::Failed { cause: reason },
            // The typed identity verdict (security.md § Pre-claim surfacing):
            // the box this claim dialed did not prove the held first-contact
            // root. Terminal — polling again re-earns the verdict forever.
            Err(ClaimAdminError::IdentityMismatch { reason }) => {
                ClaimOutcome::Failed { cause: reason }
            }
            Err(ClaimAdminError::InvalidIdentity { cause }) => ClaimOutcome::Failed { cause },
            // A misprovisioned (unreadable) claim code is terminal.
            Err(ClaimAdminError::Misprovisioned { cause }) => ClaimOutcome::Failed { cause },
            // Race: the probe above saw `claimed=false`, but the box was claimed
            // between then and this dispatch. Rare — the common already-claimed
            // recovery edge is the pre-check above, which never reaches
            // `claim_admin` at all.
            Err(ClaimAdminError::AlreadyClaimed) => ClaimOutcome::Failed {
                cause: "already_claimed".into(),
            },
        }
    }
}

// ── nat_mode_choice actions ────────────────────────────────────────────

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl OnboardingMachine {
    /// Wire-up for the `nat-mode-confirm-button`. Commits the admin's NAT-axis
    /// choice via the **mutable** `fauna.setup.nat_mode` kind (pre-identity,
    /// Ed25519-signed over `mode_wire_str ‖ "\n" ‖ actor_id_hex ‖ "\n" ‖
    /// timestamp_decimal` — the same signed envelope as the storage-mode
    /// commit; any valid admin-signed set upserts the `nest_nat_mode` row, no
    /// conflict reply). On success the wizard exits: state → `Done`,
    /// `wizard_outcome() == LoggedIn`, returns `OnboardingStep::Done`. On a
    /// 4xx-class reject the snapshot moves to `Error { transient: false }`; on
    /// transport/internal failure `Error { transient: true }` — submit stays
    /// enabled either way (resubmit allowed). Per
    /// `docs/goal/behavior/onboarding.md` § 3b-bis.
    pub async fn submit_nat_mode_choice(&self) -> OnboardingStep {
        use crate::nest_api::NatModeError;

        // Snapshot the selected mode and transition to Submitting.
        let selected_mode = {
            let mut snap = self.nat_mode.lock().unwrap();
            snap.state = NatModeState::Submitting;
            snap.message = LocalizedText {
                key: "onboarding.nat_mode.submitting".into(),
                args: Default::default(),
            };
            snap.submit_enabled = false;
            snap.selected_mode
        };
        self.observer.on_changed();

        let secret = match self.effective_secret() {
            Some(s) => s,
            None => {
                return self.nat_mode_error(
                    false,
                    "no identity".into(),
                    "onboarding.nat_mode.error.terminal",
                );
            }
        };
        // The impl signs on the commit connection (the shared ceremony, also
        // the admin panel's): the nest-bound V2 body needs the identity of the
        // box the connection reaches, learned there (`build_signed_nat_mode_body`
        // via `WsRpcNestApi::submit_nat_mode`). An unparseable secret comes
        // back as the terminal `Invalid`.
        let nest_url = self.effective_nest_url();

        match self
            .nest_api
            .submit_nat_mode(&nest_url, &secret, selected_mode)
            .await
        {
            Ok(()) => {
                {
                    let mut snap = self.nat_mode.lock().unwrap();
                    snap.state = NatModeState::Done;
                    snap.message = LocalizedText {
                        key: "onboarding.nat_mode.done".into(),
                        args: Default::default(),
                    };
                    snap.submit_enabled = false;
                }
                self.leave_nat_mode_choice()
            }
            Err(NatModeError::Invalid { reason }) => {
                self.nat_mode_error(false, reason, "onboarding.nat_mode.error.terminal")
            }
            Err(NatModeError::Transient { cause }) => {
                self.nat_mode_error(true, cause, "onboarding.nat_mode.error.transient")
            }
            // The typed identity verdict (security.md § Pre-claim surfacing):
            // terminal, never a resubmit invitation.
            Err(NatModeError::IdentityMismatch { reason }) => {
                self.nat_mode_error(false, reason, "onboarding.nat_mode.error.terminal")
            }
        }
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl OnboardingMachine {
    /// Whether the deployment's **mail** subsystem should be enabled at claim.
    ///
    /// **Machine-derived — there is no checkbox** (`onboarding.md` § 3b: the
    /// four claim-time enablement intents survive the retired
    /// `encryption_mode_choice` page as derived defaults, *not relocated* onto
    /// § 3b-bis, which stays confirm-only by design). The admin's change surface
    /// after onboarding is the admin-mail page.
    ///
    /// The value is the conjunction of two axes (ratified 2026-07-13): the
    /// handle-locality predicate ([`Self::handle_targets_real_domain`] — OFF
    /// for `user@localhost` / `user@IP`, which cannot hold MX/DKIM/TLS) AND
    /// the NAT axis ([`Self::effective_node_mode`]` != Private`). The NAT
    /// conjunct is how the two-box home-relay deployment
    /// (`deployment-home-with-public-relay.md`) says "this box runs no mail"
    /// with no checkbox: both boxes share one real-domain handle, but the home
    /// box is the one on the private axis — a claim-time enable there would
    /// mint a fresh MSEK, diverging from the fleet MSEK `LinkBoth` re-seals
    /// onto it. Still the onboarding→launch **hand-off channel**: the
    /// per-app launch glue reads this once `wizard_outcome() == LoggedIn` and,
    /// with its now-authenticated Admin client, calls
    /// `MailAdminClient::set_mail_enabled(true)` — keeping the Admin-class gate
    /// intact rather than opening a pre-identity admin surface. Idempotent with
    /// the mail-settings enable path; only meaningful on the admin-claim path.
    /// Per `docs/goal/behavior/mail-bridge-lifecycle.md` § Default-off on first
    /// claim.
    pub fn email_enable_requested(&self) -> bool {
        self.claim_completed()
            && self.handle_targets_real_domain()
            && self.effective_node_mode() != NodeMode::Private
    }

    /// Whether the deployment's **calendar (CalDAV)** subsystem should be
    /// enabled at claim. Machine-derived sibling of
    /// [`Self::email_enable_requested`] — same two-axis derivation
    /// (handle locality AND NAT axis), no checkbox (`onboarding.md` § 3b; the
    /// DAV enables also mint the shared MSEK when first, so the home-relay
    /// divergence argument covers them too).
    ///
    /// The per-app launch glue calls `MailAdminClient::set_caldav_enabled(true)`
    /// on it at `LoggedIn`. The subsystems stay **separately gated** (CalDAV needs
    /// only the HTTPS surface — no MX/DKIM — so it can be on where email is off);
    /// they merely share this default today. Per
    /// `docs/goal/behavior/caldav-server.md` § Independent enablement.
    pub fn caldav_enable_requested(&self) -> bool {
        self.claim_completed()
            && self.handle_targets_real_domain()
            && self.effective_node_mode() != NodeMode::Private
    }

    /// Whether the deployment's **contacts (CardDAV)** subsystem should be
    /// enabled at claim. Machine-derived sibling of
    /// [`Self::caldav_enable_requested`] — same two-axis derivation (handle
    /// locality AND NAT axis), no checkbox (`onboarding.md` § 3b).
    ///
    /// The per-app launch glue calls `MailAdminClient::set_carddav_enabled(true)`
    /// on it at `LoggedIn` and, when neither email nor CalDAV minted the shared
    /// MSEK, provisions the CardDAV-only mailbox
    /// (`enable_carddav_mailbox_with_generated_password`). Per
    /// `docs/goal/behavior/carddav-server.md` § Independent enablement.
    pub fn carddav_enable_requested(&self) -> bool {
        self.claim_completed()
            && self.handle_targets_real_domain()
            && self.effective_node_mode() != NodeMode::Private
    }

    /// Whether the deployment's **files (WebDAV)** subsystem should be enabled
    /// at claim. Machine-derived sibling of [`Self::carddav_enable_requested`] —
    /// same two-axis derivation (handle locality AND NAT axis), no checkbox
    /// (`onboarding.md` § 3b).
    ///
    /// The per-app launch glue calls `MailAdminClient::set_webdav_enabled(true)`
    /// on it at `LoggedIn`. Per `docs/goal/behavior/webdav-server.md`
    /// § Independent enablement.
    pub fn webdav_enable_requested(&self) -> bool {
        self.claim_completed()
            && self.handle_targets_real_domain()
            && self.effective_node_mode() != NodeMode::Private
    }

    /// User picked Public or Private on the `nat_mode_choice` page
    /// (`public-nat-mode-radio` / `private-nat-mode-radio`). Sets
    /// `selected_mode` and returns the snapshot to `Choosing` — including from
    /// `Error` (re-picking recovers); submit stays enabled.
    /// Per `docs/goal/behavior/onboarding.md` § 3b-bis.
    pub fn select_nat_mode(&self, mode: NodeMode) {
        {
            let mut snap = self.nat_mode.lock().unwrap();
            snap.selected_mode = mode;
            snap.state = NatModeState::Choosing;
            snap.message = LocalizedText {
                key: "onboarding.nat_mode.choosing".into(),
                args: Default::default(),
            };
            snap.submit_enabled = true;
        }
        self.observer.on_changed();
    }

    /// Wire-up for the `nat-mode-defer-button`: exit the `nat_mode_choice`
    /// page without committing. The seeded mode stays in effect server-side
    /// (already a working default — nothing is sent); the admin can set it
    /// later from the admin panel. The wizard exits exactly as a successful
    /// submit does: `wizard_outcome() == LoggedIn`, step → `Done`. Per
    /// `docs/goal/behavior/onboarding.md` § 3b-bis.
    pub fn defer_nat_mode_choice(&self) -> OnboardingStep {
        self.leave_nat_mode_choice()
    }

    /// App capability declaration: this app has the `trust_prompt` onboarding
    /// screen built (`onboarding.md` § 3b-ter). Call once at wizard
    /// construction; the machine then routes every *arrival* exit through the
    /// one-tap trust offer — both NAT-page exits on the claim path, and both
    /// join routes (invite redemption, approved request). Apps that never call
    /// it keep the pre-existing straight-to-`Done` flow — the
    /// batched-trickle-down parity gap, exactly as
    /// [`Self::set_renders_recovery_kit`] works.
    pub fn set_renders_trust_prompt(&self, renders: bool) {
        self.mutate(|s| s.renders_trust_prompt = renders);
    }

    /// `trust-box-grant-button`: the user trusts this box with the default
    /// grant set. Latches the answer for the signed-in handoff — the wizard
    /// holds no authenticated session, so it cannot mint here — and concludes
    /// the wizard exactly as the NAT step would have.
    pub fn grant_default_trust(&self) -> OnboardingStep {
        self.mutate(|s| s.trust_prompt_granted = true);
        self.finish_from_trust_prompt()
    }

    /// `trust-box-skip-button`: decline. "Declining leaves everything as
    /// today" (§ 3b-ter) — nothing is latched, nothing is minted, and the
    /// wizard concludes identically.
    pub fn skip_trust_prompt(&self) -> OnboardingStep {
        self.finish_from_trust_prompt()
    }

    /// Consume the `trust_prompt` answer at the wizard's signed-in handoff —
    /// the one point the client holds an authenticated session and the nest's
    /// content-processor roster, i.e. the only place
    /// `LinkedNestsAction::MintDefaultSet` can run. Consume-once (mirrors
    /// [`Self::take_pending_recovery_secret`]), so a handoff that runs twice
    /// mints once. `false` when the user skipped, never asked, or already
    /// handed off.
    pub fn take_trust_prompt_granted(&self) -> bool {
        self.mutate(|s| std::mem::take(&mut s.trust_prompt_granted))
    }
}

/// Whether a route arriving at `LoggedIn` makes § 3b-ter's one-tap trust offer
/// on the way — the wizard's answer to "is this user *joining* this box?".
///
/// `onboarding.md` § 3b-ter offers the grant "after a successful admin claim,
/// **and** at a joining user's first login", and `docs/features/join-a-nest.md`
/// outcome 8 sharpens the second half to "your first sign-in to a nest you
/// **joined**". So the axis is not "did the wizard end in a session" but "did
/// *this run* put the user on this nest": a claim and both join routes did, a
/// returning sign-in did not. It is an explicit argument rather than something
/// derived here because the derivation has burned this machine before —
/// `OnboardingMachine::claim_completed` exists precisely because an `AlreadyOnNest`
/// sign-in inferring claim-time state fired four deployment writes against a
/// box the admin had already configured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TrustOffer {
    /// This run established the user on the nest: an admin claim, an invite
    /// redemption, or an approved join request. Offer the grant.
    Offer,
    /// No offer: the user was already on this box — a returning
    /// `AlreadyOnNest` sign-in, where § 3b-ter's "first login" is long past
    /// and re-asking every launch would train the user to dismiss a
    /// capability-grant prompt. The one suppressed route. (The manual-DNS
    /// `already_claimed` resume is NOT one: it skips the wizard's setup tail,
    /// but it is this admin's own claim concluding for the first time —
    /// `complete_manual_dns_claim`.)
    Suppress,
}

impl OnboardingMachine {
    /// Internal: the claim path's exit from `nat_mode_choice`
    /// (`onboarding.md` § 3b-bis → § 3b-ter). Both NAT-page buttons end the
    /// *setup* sequence identically, so neither decides anything here — they
    /// hand the one shared exit the claim's identity and its offer verdict.
    fn leave_nat_mode_choice(&self) -> OnboardingStep {
        let (nest_url, handle) = self.logged_in_outcome_values();
        self.leave_for_logged_in(nest_url, handle, TrustOffer::Offer)
    }

    /// Internal: **the one exit every route to `LoggedIn` goes through**, and
    /// the only place § 3b-ter's one-tap trust offer is routed from.
    ///
    /// Before the joiner half landed there were four hand-written exits — this
    /// one plus three inline `WizardOutcome::LoggedIn` assignments in the
    /// handle-check, recheck-poll and redeem arms — and only this one knew the
    /// offer existed, which is exactly why three of the four never made it.
    /// Every route now states its identity and its verdict at its own call
    /// site, and the routing lives here once.
    ///
    /// When the offer is due *and* the app declared the screen, the wizard
    /// parks and the outcome stays **unset**: setting `LoggedIn` here would let
    /// an app that routes off `wizard_outcome()` rather than `step` swap to the
    /// main UI while the interstitial is still on the glass — the stale-outcome
    /// class [`Self::reset`]'s comment describes, in the other direction. The
    /// pair is captured now rather than re-derived when the user answers; see
    /// `State::trust_prompt_pending_outcome` for why that is load-bearing.
    ///
    /// Parking after a *joiner* route means the account already exists on the
    /// nest while the wizard is unfinished. That is not a new exposure and not
    /// an unrecoverable state (`principles.md` § No client-causable
    /// unrecoverable nest state): the claim path has parked past its own
    /// irreversible commit since the screen shipped, and a force-quit here
    /// relaunches into a handle check that reads `AlreadyOnNest` and signs the
    /// user straight in.
    fn leave_for_logged_in(
        &self,
        nest_url: String,
        handle: String,
        offer: TrustOffer,
    ) -> OnboardingStep {
        if offer == TrustOffer::Offer && self.with_state(|s| s.renders_trust_prompt) {
            self.mutate(|s| {
                s.trust_prompt_pending_outcome = Some((nest_url, handle));
                s.step = OnboardingStep::TrustPrompt;
            });
            return OnboardingStep::TrustPrompt;
        }
        self.finish_logged_in(nest_url, handle)
    }

    /// Internal: conclude the wizard — `wizard_outcome() == LoggedIn`, step →
    /// `Done`. Every caller arrives through [`Self::leave_for_logged_in`].
    fn finish_logged_in(&self, nest_url: String, handle: String) -> OnboardingStep {
        *self.outcome.lock().unwrap() = Some(WizardOutcome::LoggedIn { nest_url, handle });
        self.state.lock().unwrap().step = OnboardingStep::Done;
        self.observer.on_changed();
        OnboardingStep::Done
    }

    /// Internal: conclude from the parked `trust_prompt`, with the identity the
    /// arriving route captured. The fallback derivation covers only a prompt
    /// reached without a park (a test injecting the step directly).
    fn finish_from_trust_prompt(&self) -> OnboardingStep {
        let (nest_url, handle) = self
            .mutate(|s| s.trust_prompt_pending_outcome.take())
            .unwrap_or_else(|| self.logged_in_outcome_values());
        self.finish_logged_in(nest_url, handle)
    }

    /// Internal: snapshot transition for the nat-mode Error variant. Returns
    /// the current step (i.e. stays on NatModeChoice). Submit stays enabled —
    /// the `fauna.setup.nat_mode` set is mutable, so resubmit is always
    /// allowed. Mirrors `encryption_mode_error`.
    fn nat_mode_error(&self, transient: bool, cause: String, message_key: &str) -> OnboardingStep {
        let mut args = HashMap::new();
        args.insert("cause".into(), cause.clone());
        {
            let mut snap = self.nat_mode.lock().unwrap();
            snap.state = NatModeState::Error { transient, cause };
            snap.message = LocalizedText {
                key: message_key.into(),
                args,
            };
            snap.submit_enabled = true;
        }
        self.observer.on_changed();
        self.state.lock().unwrap().step
    }
}

impl OnboardingMachine {
    /// Internal: snapshot transition for the Error variant. Returns the
    /// current step (i.e. stays on ClaimCode). The message_key is the
    /// i18n string the page should render — per `LocalizedText` shape.
    fn claim_code_error(
        &self,
        transient: bool,
        cause: String,
        message_key: &str,
    ) -> OnboardingStep {
        use crate::snapshots::ClaimCodeState;
        let mut args = HashMap::new();
        args.insert("cause".into(), cause.clone());
        {
            let mut snap = self.claim_code.lock().unwrap();
            snap.state = ClaimCodeState::Error { transient, cause };
            snap.message = LocalizedText {
                key: message_key.into(),
                args,
            };
            snap.submit_enabled = true;
        }
        self.observer.on_changed();
        self.state.lock().unwrap().step
    }

    /// Internal: the nest genuinely already has an admin — no different code
    /// would succeed, so this gets its own dedicated, translatable message
    /// (`onboarding.claim_code.error.already_claimed`) instead of the generic
    /// `onboarding.claim_code.invalid`'s `{reason}` substitution, which would
    /// otherwise show the raw wire error code verbatim. Still structurally
    /// `Invalid` (submit re-enabled; `claim-code-back-button` is the real exit
    /// — the code was never the problem, so there is nothing to "fix and
    /// retry", but the page must not get stuck).
    fn claim_code_already_claimed(&self) -> OnboardingStep {
        use crate::snapshots::ClaimCodeState;
        {
            let mut snap = self.claim_code.lock().unwrap();
            snap.state = ClaimCodeState::Invalid {
                reason: "already_claimed".into(),
            };
            snap.message = LocalizedText {
                key: "onboarding.claim_code.error.already_claimed".into(),
                args: Default::default(),
            };
            snap.submit_enabled = true;
        }
        self.observer.on_changed();
        self.with_state(|s| s.step)
    }

    /// Internal: set the claim-code snapshot to `Invalid { reason }` (the
    /// retry-able 4xx-class rejection — bad code, missing or invalid handle)
    /// with the shared `onboarding.claim_code.invalid` message, fire an
    /// observer tick, and return the (unchanged) current step. Used both for
    /// the nest's `ClaimAdminError::Invalid` and for the client-side
    /// required-handle guard, so the UX is identical whichever catches it.
    fn claim_code_invalid(&self, reason: impl Into<String>) -> OnboardingStep {
        use crate::snapshots::ClaimCodeState;
        let reason = reason.into();
        let mut args = HashMap::new();
        args.insert("reason".into(), reason.clone());
        {
            let mut snap = self.claim_code.lock().unwrap();
            snap.state = ClaimCodeState::Invalid { reason };
            snap.message = LocalizedText {
                key: "onboarding.claim_code.invalid".into(),
                args,
            };
            snap.submit_enabled = true;
        }
        self.observer.on_changed();
        self.with_state(|s| s.step)
    }

    /// Internal: set the "Almost ready" snapshot's state + i18n message and
    /// fire an observer tick. Preserves `dns_records` (set when the surface
    /// was entered). Used by `recheck_manual_dns`.
    fn set_awaiting_dns_state(&self, state: AwaitingDnsState, message_key: &str) {
        {
            let mut snap = self.awaiting_manual_dns.lock().unwrap();
            snap.state = state;
            snap.message = LocalizedText {
                key: message_key.into(),
                args: Default::default(),
            };
        }
        self.observer.on_changed();
    }

    /// Internal: terminal-error transition for the "Almost ready" surface.
    /// Leaves `wizard_outcome()` as `AwaitingManualDns` so a relaunch resumes
    /// here; returns the current (unchanged) step. Transient failures must
    /// NOT route here — they return to `Pending` so polling continues.
    fn awaiting_dns_error(&self, cause: String) -> OnboardingStep {
        {
            let mut snap = self.awaiting_manual_dns.lock().unwrap();
            snap.state = AwaitingDnsState::Error {
                cause: cause.clone(),
            };
            let mut args = HashMap::new();
            args.insert("cause".into(), cause);
            snap.message = LocalizedText {
                key: "onboarding.awaiting_dns.error".into(),
                args,
            };
        }
        self.observer.on_changed();
        self.with_state(|s| s.step)
    }

    /// Internal: shared completion for a successful claim (or an already-
    /// claimed nest) reached from `recheck_manual_dns`. Sets the snapshot to
    /// `Claimed` and routes on **who claimed the box**, not on any storage mode
    /// — a no-modes nest has no unresolved setup state to branch on
    /// (`onboarding.md` § "Almost ready"; `storage-modes.md` § Boot story):
    ///
    /// * `already_claimed` — the recovery edge (we claimed, then the app
    ///   restarted before the wizard exited). The box is fully set up, so
    ///   skip the setup tail (`NatModeChoice`) and conclude without
    ///   re-claiming — through § 3b-ter's offer, which is not a setup step
    ///   (`onboarding.md` § 3b-ter, ratified 2026-09-21).
    /// * fresh claim — advance to `NatModeChoice`, **exactly the path
    ///   `wizard_submit_claim_code` takes**, clearing the awaiting outcome so
    ///   the client re-routes off the "Almost ready" surface.
    fn complete_manual_dns_claim(&self, already_claimed: bool) -> OnboardingStep {
        self.set_awaiting_dns_state(AwaitingDnsState::Claimed, "onboarding.awaiting_dns.claimed");
        self.forget_pending_provision_row();
        // The claim axis of the § 3b derivation — set on BOTH arms. The
        // `already_claimed` arm is a claim too: it is this admin's own
        // interrupted run resuming (claimed, then the app restarted before the
        // wizard exited), so its launch glue never ran and its claim-time
        // enablement is still owed. What must NOT reach here is a plain
        // `AlreadyOnNest` sign-in, which routes through
        // `submit_handle_check_continue` and never calls this at all.
        self.mutate(|s| s.claim_completed = true);
        if already_claimed {
            let (nest_url, handle) = self.with_state(|s| (s.nest_url.clone(), s.handle.clone()));
            // `Offer` (§ 3b-ter, ratified 2026-09-21). This arm is the
            // *recovery edge* and keeps skipping the NAT page (§ 3b-bis): that
            // is a setup step whose nest-held seed already holds without it.
            // The offer is not a setup step and has no seeded answer — and the
            // one thing that decides the offer on every route is whether the
            // wizard is concluding, for the first time, a run that put the
            // user on this box. This one is: the admin's own claim, whose
            // wizard never exited, so the admin was never asked. Skipping the
            // setup tail is no reason to skip the ask. A force-quit on the
            // offer itself never wedges: every app clears the awaiting slot
            // at `LoggedIn` and nowhere earlier (`persist_logged_in`;
            // `onboarding.md` § Long-term store contract, ratified
            // 2026-09-21), so the relaunch comes back into this arm and asks
            // once more — the recovery an earlier clear (at the claim) would
            // have traded for a lost offer.
            //
            // Clear the awaiting outcome first, as the fresh-claim arm does:
            // while the offer is parked the outcome must be unset (an app
            // routing off `wizard_outcome()` would otherwise stay on the
            // "Almost ready" surface), and `recheck_manual_dns` proceeds only
            // from `AwaitingManualDns`, so the app's poll timer cannot re-enter
            // and re-park behind the interstitial. Without the screen,
            // `leave_for_logged_in` sets `LoggedIn` straight away.
            *self.outcome.lock().unwrap() = None;
            return self.leave_for_logged_in(nest_url, handle, TrustOffer::Offer);
        }
        self.reset_nat_mode_snapshot();
        *self.outcome.lock().unwrap() = None;
        self.mutate(|s| s.step = OnboardingStep::NatModeChoice);
        OnboardingStep::NatModeChoice
    }
}

// Internal helpers — not exported via UniFFI.
impl OnboardingMachine {
    /// The age claim the next admission puts on the wire — the one enforcement
    /// point of `family-safety.md` § The account age band → *An attestation
    /// the nest cannot check*. The stored claim, with its attestation kept
    /// only when the last [`Self::request_age_nonce`] mint was from the nest
    /// the admission goes to, minted exactly the nonce the attestation binds,
    /// and listed the attestation's platform; otherwise the claim rides
    /// declared-only. Both admission bodies and the onboarding notice read it.
    fn age_claim_to_send(&self) -> Option<fauna_protocol::age::AgeClaim> {
        let mut claim = self
            .age_claim
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        if let Some(attestation) = &claim.attestation {
            let mint = self
                .age_nonce_mint
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let checkable = mint.as_ref().is_some_and(|m| {
                m.nest_url == self.effective_nest_url()
                    && m.nonce_hex == attestation.nonce
                    && m.attestation_platforms.contains(&attestation.platform)
            });
            if !checkable {
                claim.attestation = None;
            }
        }
        Some(claim)
    }

    /// Synchronous read accessor. Acquires the std::sync::Mutex briefly,
    /// returns the projection, releases.
    pub(crate) fn with_state<R>(&self, f: impl FnOnce(&State) -> R) -> R {
        let guard = self.state.lock().unwrap();
        f(&guard)
    }

    /// Synchronous mutation: lock, mutate, notify observer.
    pub(crate) fn mutate<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let result = {
            let mut guard = self.state.lock().unwrap();
            f(&mut guard)
        };
        self.observer.on_changed();
        result
    }

    /// Take an owned snapshot of the state without mutating it. Used by async
    /// methods that need to read several fields at once before doing IO.
    pub(crate) fn snapshot(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    /// Clear every handle-check artifact so the HandleEntry page starts fresh
    /// after the identity changes — both the rich snapshot the page renders
    /// (`self.handle_check`) and the `State` fields a completed probe writes
    /// (`State::clear_handle_check`). Called from the identity-confirm
    /// transitions (`confirm_generated_identity` / `confirm_imported_identity`):
    /// without it, a prior probe's outcome (e.g. "unregistered at example.com")
    /// survives an identity re-import/re-create and the page shows a stale
    /// conclusion until the user clicks Check again. The handle text itself is
    /// preserved — only the probe result is reset. Fires one observer tick (via
    /// `mutate`); the bare `handle_check` reset just above it does not observe.
    fn reset_handle_check(&self) {
        *self.handle_check.lock().unwrap() = HandleCheckSnapshot::idle();
        self.mutate(|s| s.clear_handle_check());
    }

    /// The box's **effective NAT mode** once the wizard has passed
    /// `nat_mode_choice`: the committed choice when `submit_nat_mode_choice`
    /// succeeded (the `Done` snapshot retains `selected_mode`), else the
    /// nest's resolved seed (`fauna.setup.status`.`node_mode`) — a defer sends
    /// nothing, so the seed stays authoritative, and an absent seed falls back
    /// to the nest's absent-row default, `Public`. This is the NAT conjunct of
    /// the serving-enablement derivation ([`Self::email_enable_requested`] +
    /// siblings; `onboarding.md` § 3b). The private-ward pre-*selection* is
    /// deliberately not consulted: it is a suggestion the confirm would
    /// commit, not the box's mode.
    fn effective_node_mode(&self) -> NodeMode {
        {
            let snap = self.nat_mode.lock().unwrap();
            if snap.state == NatModeState::Done {
                return snap.selected_mode;
            }
        }
        self.with_state(|s| s.node_mode_seed)
            .unwrap_or(NodeMode::Public)
    }

    /// True **iff** this wizard run completed an admin claim of the box — the
    /// **claim axis** of the § 3b serving-enablement derivation
    /// ([`Self::email_enable_requested`] + siblings), and the first conjunct of
    /// all four.
    ///
    /// § 3b is "Serving enablement **at claim**": its two published axes say how
    /// the intents *default* once a claim has happened, not whether one did. The
    /// wizard reaches `WizardOutcome::LoggedIn` from three routes — admin claim,
    /// `AlreadyOnNest` **sign-in**, and invite redemption — and every app's
    /// launch glue reads these getters at that one outcome, so without this
    /// conjunct a returning admin merely signing in on a real-domain public box
    /// derived all four ON and the glue fired four Admin-class deployment writes
    /// against a box they had already configured. Observed against the live box
    /// 2026-08-16; pinned by
    /// `tests/serving_enablement_derivation.rs::plain_sign_in_on_a_real_domain_public_box_derives_all_four_off`.
    fn claim_completed(&self) -> bool {
        self.with_state(|s| s.claim_completed)
    }

    /// True **iff** the handle's domain is a real registerable domain — not a
    /// loopback / `*.localhost` or IP-literal target. This is the handle-
    /// locality axis of the § 3b serving-enablement derivation
    /// ([`Self::email_enable_requested`] + siblings): every subsystem needs a
    /// publicly-reachable domain (email wants MX/DKIM/public TLS, CalDAV wants
    /// a publicly-trusted cert for `mail.<domain>` + DNS), so a
    /// `user@localhost` / `user@<ip>` deployment derives everything OFF; the
    /// admin's change surface is the post-onboarding settings pages. Reuses
    /// `fauna_provisioning::probe::resolve_handle_domain().is_public_dns_name`
    /// — the same loopback/IP predicate the handle-check probe uses (so
    /// onboarding has one definition of "local target").
    /// Per `docs/goal/behavior/onboarding.md`
    /// §3b and `docs/goal/behavior/caldav-server.md` § Independent enablement.
    fn handle_targets_real_domain(&self) -> bool {
        self.with_state(|s| match s.handle_domain() {
            Some(domain) => {
                fauna_provisioning::probe::resolve_handle_domain(domain).is_public_dns_name
            }
            None => false,
        })
    }

    /// True **iff** the handle's domain targets a **private network** — a
    /// `localhost`/`*.localhost` or `.local` name, or a non-global IP literal
    /// (`fauna_core::resolve::is_private_network_target`; a *public* bare IP
    /// does NOT qualify). Any `:port` is stripped with the same splitter the
    /// handle-check probe uses (`fauna_provisioning::probe::split_host_port`),
    /// so `192.168.1.50:8443` classifies by its host. This is the
    /// reachability-flavored classifier behind the NAT-mode private-ward
    /// refinement — deliberately distinct from
    /// [`Self::handle_targets_real_domain`]'s `is_public_dns_name` locality
    /// axis. Per `docs/goal/behavior/onboarding.md` § 3b-bis Defaulting note.
    fn handle_targets_private_network(&self) -> bool {
        self.with_state(|s| match s.handle_domain() {
            Some(domain) => {
                let (host, _port) = fauna_provisioning::probe::split_host_port(domain);
                fauna_core::resolve::is_private_network_target(host)
            }
            None => false,
        })
    }

    /// Reset the `nat_mode_choice` snapshot on entry to the page, deriving the
    /// pre-selection so the common case is confirm-only (onboarding.md
    /// § 3b-bis Defaulting note):
    ///
    /// 1. A `Private` seed always pre-selects Private — only the home-relay
    ///    installer sets it, always deliberately; never overridden the other
    ///    way (the refinement is one-directional).
    /// 2. A `Public` (or absent) seed — which is also just the generic-image
    ///    default — is refined **private-ward** when the handle targets a
    ///    private network: pre-select Private with the
    ///    `onboarding.nat_mode.private_hint` message explaining why.
    /// 3. Otherwise Public with the plain `choosing` message.
    ///
    /// No inference is authoritative — the human on the page decides.
    fn reset_nat_mode_snapshot(&self) {
        let seed = self.with_state(|s| s.node_mode_seed);
        let mut snap = NatModeSnapshot::idle();
        if seed == Some(NodeMode::Private) {
            snap.selected_mode = NodeMode::Private;
        } else if self.handle_targets_private_network() {
            snap.selected_mode = NodeMode::Private;
            snap.message = LocalizedText {
                key: "onboarding.nat_mode.private_hint".into(),
                args: Default::default(),
            };
        }
        *self.nat_mode.lock().unwrap() = snap;
    }

    /// The `(nest_url, handle)` pair a `WizardOutcome::LoggedIn` carries: the
    /// persisted `state.nest_url` when set (the value that identifies the nest
    /// to per-app glue), else the effective URL (test override / derived).
    /// Factored out of the pre-3b-bis `submit_encryption_mode_choice` success
    /// arm so `submit_nat_mode_choice` and `defer_nat_mode_choice` exit with
    /// exactly the values that arm used to produce.
    fn logged_in_outcome_values(&self) -> (String, String) {
        let persisted = self.state.lock().unwrap().nest_url.clone();
        let nest_url = if persisted.is_empty() {
            self.effective_nest_url()
        } else {
            persisted
        };
        (nest_url, self.current_handle())
    }
}

// Test-only impl block split in two:
//   1. The block below carries the closure-taking `set_dns_state_for_test`
//      method which UniFFI cannot export (closures have no FFI ABI). Stays
//      Rust-internal (in-process tests + Linux native consumers).
//   2. The second block (further down) carries FFI-friendly setters. It's
//      `uniffi::export`-decorated when both `uniffi` and `test-helpers`
//      features are on, so the cross-app E2E bridge can reach
//      `set_handle_check_snapshot_for_test` etc.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl OnboardingMachine {
    /// Test-only constructor that accepts an explicit `NestApi` impl,
    /// bypassing the production `WsNestApi`. Used by lifecycle tests that
    /// fixture nest responses with `FakeNestApi` instead of standing up a
    /// server.
    ///
    /// All other fields default to the same values `new(observer, None)`
    /// produces. The `http` client is still created so non-trait
    /// (cloud-provider / DNS / nest-health-probe) HTTP calls keep
    /// working; if a test exercises a code path that hits one of those,
    /// it should use `new(observer, Some(provider_base_urls))` instead.
    ///
    /// Test-only constructor carrying the provider base-URL override map
    /// (provider id — `"vps"`, `"dns"`, `"nest"` — → base URL), which redirects
    /// the VPS / DNS / nest-health HTTP calls to the Python fake at
    /// `tests/e2e-unified/fakes/fake_cloud.py`.
    ///
    /// This is the **construction-time** channel, and it is the one web needs:
    /// the SPA reconstructs the wizard machine on reload, and only a
    /// construction-time map reaches `WsNestApi`'s `base_url_override` (the
    /// runtime `set_provider_base_urls` below is HTTP-only by design). Native
    /// apps hold one long-lived machine and use the runtime setter instead.
    ///
    /// Not exported via UniFFI — a production artifact must carry no way at all
    /// to install the map (`testing.md` convention 15). Its visibility gate is
    /// the convention's own `any(test, debug_assertions, feature =
    /// "test-helpers")`, which is what this block already carries.
    /// The `"nest"` entry is mirrored into the launch-side dial seam here for
    /// the same reason [`Self::set_provider_base_urls`] does it: web installs
    /// the map through *this* constructor, not the runtime setter, so without
    /// the mirror web would be the one app whose store-read dial never resolved.
    pub fn new_with_provider_base_urls(
        observer: Arc<dyn OnboardingObserver>,
        provider_base_urls: Option<HashMap<String, String>>,
    ) -> Arc<Self> {
        fauna_launch_machine::set_nest_dial_override(
            provider_base_urls
                .as_ref()
                .and_then(|urls| urls.get("nest").cloned()),
        );
        Self::build(observer, provider_base_urls)
    }

    /// Not exported via UniFFI — `Arc<dyn NestApi>` has no FFI ABI.
    pub fn with_nest_api(
        observer: Arc<dyn OnboardingObserver>,
        nest_api: Arc<dyn crate::nest_api::NestApi>,
    ) -> Arc<Self> {
        Self::assemble(
            observer,
            nest_api,
            RwLock::new(None),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            // Handed its own transport, so it inherits no process-global dial
            // override: see the `inherits_dial_override` field's note.
            false,
        )
    }

    /// Both halves at once: fake cloud providers **and** a fake nest.
    ///
    /// A provisioning run now ends in a claim against the box it just built
    /// (`onboarding.md` § 6 *Provisioning = build + claim*), so a test that drives
    /// `start_provisioning` to `Succeeded` needs a nest to claim as well as
    /// providers to build with — the provider-URL constructor alone leaves the
    /// claim dialling a mock HTTP server over WS-RPC, which is exactly what an
    /// unreachable box looks like. Not exported via UniFFI, for the same reason
    /// [`Self::with_nest_api`] is not.
    pub fn with_nest_api_and_provider_base_urls(
        observer: Arc<dyn OnboardingObserver>,
        nest_api: Arc<dyn crate::nest_api::NestApi>,
        provider_base_urls: Option<HashMap<String, String>>,
    ) -> Arc<Self> {
        fauna_launch_machine::set_nest_dial_override(
            provider_base_urls
                .as_ref()
                .and_then(|urls| urls.get("nest").cloned()),
        );
        Self::assemble(
            observer,
            nest_api,
            RwLock::new(provider_base_urls),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            // Same reasoning as `with_nest_api`: its own transport, its own map,
            // no inheritance from the process global.
            false,
        )
    }

    /// Test-only: mutate the dns config sub-state directly. Lets pure-
    /// computation tests inject zone lists / availability / contact / etc.
    /// without standing up a wiremock server for `verify_dns`.
    /// Not UniFFI-exportable (closure parameter); FFI clients use
    /// `set_dns_creds` and `set_domain_status_for_test` instead.
    pub fn set_dns_state_for_test(&self, mutator: impl FnOnce(&mut crate::state::DnsConfigState)) {
        self.mutate(|s| mutator(&mut s.dns));
    }

    /// Test-only: seed a verified DNS-provider selection plus
    /// `dns.buy_domain` + `dns.current_availability` so `provider_status()`
    /// reaches `UnregisteredBuyable` — the domain-line twin of
    /// `set_vps_state_for_test`, for fixturing `bill_of_materials()`'s
    /// optional domain-registration line without driving the real
    /// `verify_dns()` registrar probe. Self-sufficient like
    /// `set_vps_state_for_test` (sets `selected_provider_id` + `verified`
    /// itself — `provider_status()` returns `NotReady` without them).
    /// FFI-reachable via `call_machine_method` (closures have no FFI ABI, so
    /// this thin wrapper is exported instead of `set_dns_state_for_test`
    /// directly).
    pub fn set_dns_availability_for_test(
        &self,
        provider_id: String,
        buy_domain: bool,
        price_cents: u64,
        currency: Option<String>,
    ) {
        self.set_dns_state_for_test(|s| {
            s.selected_provider_id = Some(provider_id);
            s.verified = true;
            s.buy_domain = buy_domain;
            s.current_availability = Some(
                fauna_provisioning::registrar::RegistrarAvailability::Buyable {
                    price_cents,
                    currency,
                    renewal_cents: None,
                },
            );
        });
    }

    /// Test-only: mutate the vps config sub-state directly. Mirrors
    /// `set_dns_state_for_test`. Used by `continue_from_vps` tests to set
    /// up `verified` / `selected_server_type_id` / `selected_location_id`
    /// without driving the verify_vps probe path.
    pub fn set_vps_state_for_test(&self, mutator: impl FnOnce(&mut crate::state::VpsConfigState)) {
        self.mutate(|s| mutator(&mut s.vps));
    }

    /// Test-only: arbitrary closure mutation of internal state. Mirrors
    /// `set_dns_state_for_test` but for the whole `State`. FFI clients
    /// can't reach this directly (closures have no FFI ABI); use
    /// individual setters instead.
    #[allow(dead_code)]
    pub(crate) fn mutate_for_test(&self, f: impl FnOnce(&mut crate::state::State)) {
        self.mutate(f);
    }

    /// Test-only: inject the zone the handle check found the domain inside
    /// (what a `DomainAvailable` probe of `dev.example.com` records), so
    /// `submit_handle_check_continue`'s `buy_domain` derivation is testable
    /// without the network probe.
    pub fn set_handle_enclosing_zone_for_test(&self, zone: Option<String>) {
        self.mutate(|s| s.handle_enclosing_zone = zone);
    }

    /// Test-only: inject the nest's resolved NAT-mode seed (as would be
    /// learned from a setup-status probe) so `nat_mode_choice` pre-selection
    /// tests can exercise the seed/private-ward defaulting without driving
    /// the full probe path. Mirrors `set_nest_mode_for_test`. Per
    /// `docs/goal/behavior/onboarding.md` § 3b-bis.
    pub fn set_node_mode_seed_for_test(&self, seed: Option<NodeMode>) {
        self.mutate(|s| s.node_mode_seed = seed);
    }

    /// Test-only readback for the captured NAT-mode seed. Mirrors
    /// `nest_mode_for_test`.
    pub fn node_mode_seed_for_test(&self) -> Option<NodeMode> {
        self.with_state(|s| s.node_mode_seed)
    }
}

/// Parse one E2E bridge method's JSON payload, `tracing::warn!`ing with the
/// method name and the serde error instead of silently dropping the call
/// when a KNOWN method name carries a malformed payload — convention 11's
/// "a recognised arm that declines must be loud too"
/// (`e2e-conventions.md` § The convention). The pre-existing silent-
/// unknown-NAME behavior at each dispatcher's `_`/`None` arm is untouched:
/// this only ever fires once a name has already matched a known
/// arm.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
fn parse_bridge_arg<T: serde::de::DeserializeOwned>(method: &str, json_arg: &str) -> Option<T> {
    match serde_json::from_str(json_arg) {
        Ok(v) => Some(v),
        Err(error) => {
            tracing::warn!(
                method,
                %error,
                "e2e bridge: malformed payload for a known method"
            );
            None
        }
    }
}

// FFI-safe test-helper setters. Exported via UniFFI when the
// `test-helpers` feature is enabled, so per-app E2E bridges can drive
// snapshot fixturing through `call_machine_method` (the E2E bridge
// contract; tracked internally).
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(all(feature = "uniffi", feature = "test-helpers"), uniffi::export)]
impl OnboardingMachine {
    /// Test-only: install the provider base-URL override map at runtime.
    ///
    /// The native E2E bridge (`call_machine_method("set_provider_base_urls", …)`)
    /// calls this so the orchestrator's HTTP calls (vps/dns/nest-health, all
    /// read via `provider_base_url` at call time) hit the `fake_cloud` fixture
    /// instead of the live internet. Mirrors web's `__fauna_setProviderBaseUrls`
    /// — except web drops the cached machine to reconstruct (through
    /// `new_with_provider_base_urls`), while native apps keep their one
    /// page-bound machine and override in place.
    ///
    /// Scope: this is the **HTTP** override. The WS-RPC `nest_api` (pre-identity
    /// handle-check / invite / claim) keeps its construction-time override —
    /// `fake_cloud` mocks HTTP only, so those paths aren't redirected here.
    ///
    /// Lives in *this* block, not the unconditionally-exported one above: it is
    /// the automation seam that installs the override, so a production artifact
    /// must not carry it at all. A per-method `#[cfg]` inside an
    /// `#[uniffi::export]`ed impl would not work — see the note on
    /// `reset_provisioning_for_test` below.
    ///
    /// The `"nest"` entry is additionally **mirrored into the process-global
    /// launch-side dial seam** (`fauna_launch_machine::dial`). This machine
    /// instance can only answer for callers that hold it, and every app but tui
    /// begins its authenticated launch at the *store* instead — a relaunch, an
    /// "Add account" switch, the post-claim launch — with no machine in scope.
    /// Mirroring keeps that one harness gesture sufficient for both halves, so
    /// no driver, agent command or app has to learn a second seam.
    pub fn set_provider_base_urls(&self, urls: HashMap<String, String>) {
        // Deliberately NOT gated more narrowly than the block it sits in: this
        // crate's `test-helpers` forwards `fauna-launch-machine/e2e-agent`
        // precisely so the two halves of ONE automation surface cannot be
        // compiled apart. A config where the wizard's override worked but the
        // launch's silently did not would be a split brain no test could see.
        fauna_launch_machine::set_nest_dial_override(urls.get("nest").cloned());
        *self
            .provider_base_urls
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(urls);
    }

    /// The URL an app should **dial** to reach the nest `nest_url` names: the
    /// `provider_base_urls["nest"]` override when one is installed, else
    /// `nest_url` unchanged.
    ///
    /// **What this exists for.** Every *pre-identity* call already resolves
    /// through that override — the handle-check probe ([`Self::probe_base`]'s
    /// `nest_override.unwrap_or(target.base_url)`) and the claim / NAT commit
    /// ([`Self::effective_nest_url`]) — but the **authenticated** session each
    /// app establishes at `WizardOutcome::LoggedIn` dialed the *literal* typed
    /// URL, which for a domain-shaped handle is an unresolvable
    /// `https://{domain}`. That is why the claim-time serving-enablement ON
    /// branch (`onboarding.md` § 3b: mail/CalDAV/CardDAV/WebDAV default ON iff
    /// the handle domain is a real public DNS name AND the NAT mode is not
    /// private) had never been e2e-proven on any app: a real-domain claim
    /// against a local nest reached `LoggedIn` and then could not connect, so
    /// the launch glue's `enable_mail_with_generated_password` never ran.
    ///
    /// **It redirects the dial, never the truth.** `state.nest_url` — and so
    /// `WizardOutcome::LoggedIn.nest_url`, what apps persist and display —
    /// stays the literal typed string; callers keep passing *that* to the
    /// registry write and to anything user-facing (tui hands the literal to
    /// `MuaInstructions::for_node_url`, which prints the MUA host). Only the
    /// socket the client opens moves.
    ///
    /// Lives in *this* block for the same reason [`Self::set_provider_base_urls`]
    /// does: it reads the automation override, so a production artifact must
    /// carry no such symbol at all (`e2e-conventions.md` point 15). With no
    /// override installed it is the identity function, which is the only
    /// behavior a production build could have had anyway.
    ///
    /// **It is a face, not a second rule.** The resolution itself lives once, in
    /// `fauna_launch_machine::dial` — the crate every app's *store-read* launch
    /// runs through, which is where the six non-tui apps need it. This method
    /// exists so a caller already holding a machine (tui's wizard glue, and the
    /// UniFFI-exported surface apple/android/windows bind) does not have to
    /// reach for a second import; delegating rather than re-reading
    /// `provider_base_urls["nest"]` is what makes the two answers unable to
    /// disagree.
    pub fn resolved_nest_dial_url(&self, nest_url: String) -> String {
        fauna_launch_machine::resolved_dial_url(&nest_url)
    }

    /// Per-test override hook used by the bridge. Resets the snapshot to
    /// `idle` without doing any IO. Production code goes through
    /// `start_provisioning` / `retry_provisioning`.
    ///
    /// Lives in *this* block, not the unconditionally-exported one above:
    /// a per-method `#[cfg]` inside an `#[uniffi::export]`ed impl is not
    /// propagated into UniFFI's generated scaffolding, so the scaffolding
    /// would call a method cfg had removed — which is what pinned
    /// `test-helpers` on `fauna-ffi`'s dep line and rode this seam into
    /// five release artifacts (`testing.md` convention 15).
    pub fn reset_provisioning_for_test(&self) {
        self.provisioning_cancel.reset();
        *self.provisioning.lock().unwrap() = ProvisioningSnapshot::idle();
        self.observer.on_changed();
    }

    /// Test-only seam for the pending-provision slot's wiring, so a test can
    /// drive a real provisioning run against an in-memory store without going
    /// through the UniFFI constructor ([`Self::new_with_persistence`]).
    ///
    /// Lives in *this* block, not the unconditionally-exported one above, for
    /// the same reason [`Self::reset_provisioning_for_test`] does — a
    /// production artifact must carry no such symbol at all.
    pub fn set_pending_provision_store_for_test(&self, store: Arc<dyn PendingProvisionStore>) {
        *self
            .pending_provision
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(store);
    }

    /// Test-only: jump straight to a given step. Bypasses transition rules
    /// so individual stage methods can be exercised in isolation.
    pub fn set_step_for_test(&self, step: OnboardingStep) {
        self.mutate(|s| s.step = step);
    }

    /// Test-only: install a wizard outcome directly, so outcome-keyed surfaces
    /// (the "Almost ready" `AwaitingManualDns` page) can be rendered without
    /// driving the full deferred-DNS path that produces them.
    pub fn set_wizard_outcome_for_test(&self, outcome: crate::outcome::WizardOutcome) {
        *self.outcome.lock().unwrap() = Some(outcome);
        self.observer.on_changed();
    }

    /// Test-only: inject a `handle_check_snapshot().outcome` so
    /// `domain_status()`/`provider_status()` (both derived from it) can
    /// branch on `DomainAvailable`/`RegisteredNoNest`/etc. without driving
    /// the full `submit_handle` probe path. Replaces the old
    /// `set_domain_status_for_test` now that `domain_status` is a derived
    /// view, not stored state — leaves the rest of the snapshot untouched.
    pub fn set_handle_check_outcome_for_test(&self, outcome: crate::snapshots::HandleCheckOutcome) {
        self.handle_check.lock().unwrap().outcome = outcome;
    }

    /// Test-only: bulk-set the DNS creds map. Accepts plain `String` values
    /// (test ergonomics) and wraps them in `SecretString` to match the field.
    pub fn set_dns_creds(&self, creds: std::collections::HashMap<String, String>) {
        self.mutate(|s| {
            s.dns.creds = creds
                .into_iter()
                .map(|(k, v)| (k, SecretString::from(v)))
                .collect();
        });
    }

    /// Test-only: inject a pre-built HandleCheckSnapshot so tests can
    /// exercise `submit_handle_check_continue` without running the full
    /// network probe sequence.
    pub fn set_handle_check_snapshot_for_test(&self, snap: HandleCheckSnapshot) {
        *self.handle_check.lock().unwrap() = snap;
        self.observer.on_changed();
    }

    pub fn set_invite_request_snapshot_for_test(&self, snap: InviteRequestSnapshot) {
        *self.invite_request.lock().unwrap() = snap;
        self.observer.on_changed();
    }

    /// Test-only: inject a pre-built `ClaimCodeSnapshot` so tests can
    /// fixture the `claim_code` page without driving a real
    /// `claim-admin` POST. Mirrors `set_invite_request_snapshot_for_test`.
    pub fn set_claim_code_snapshot_for_test(&self, snap: ClaimCodeSnapshot) {
        *self.claim_code.lock().unwrap() = snap;
        self.observer.on_changed();
    }

    /// Test-only: inject a pre-built `NatModeSnapshot` so tests can fixture
    /// the `nat_mode_choice` page without driving a real
    /// `fauna.setup.nat_mode` commit. Mirrors
    /// `set_claim_code_snapshot_for_test`.
    pub fn set_nat_mode_snapshot_for_test(&self, snap: NatModeSnapshot) {
        *self.nat_mode.lock().unwrap() = snap;
        self.observer.on_changed();
    }

    /// Test-only: inject a pre-built `AwaitingManualDnsSnapshot` so tests
    /// can fixture the "Almost ready" surface without driving the deferred-
    /// DNS provisioning path. Mirrors `set_claim_code_snapshot_for_test`.
    pub fn set_awaiting_manual_dns_snapshot_for_test(&self, snap: AwaitingManualDnsSnapshot) {
        *self.awaiting_manual_dns.lock().unwrap() = snap;
        self.observer.on_changed();
    }

    /// Test-only: inject a DNS records list as if the deferred-DNS
    /// orchestrator had populated it. Used by `test_provisioning_progress`
    /// to fixture the AwaitingManualDns exit path without running the real
    /// orchestrator.
    pub fn set_dns_records_for_test(&self, records: Vec<DnsRecordPlain>) {
        self.mutate(|s| s.dns_records = records);
    }

    /// Test-only: drop a `ProvisioningSnapshot` directly into the machine.
    /// Used by the cross-app E2E bridge to fixture step transitions
    /// (Running, Failed, Succeeded, Cancelled) without driving the real
    /// orchestrator. Callers typically also
    /// `set_step_for_test("NestProvisioning")` so the page actually
    /// renders.
    pub fn set_provisioning_snapshot_for_test(&self, snap: ProvisioningSnapshot) {
        *self.provisioning.lock().unwrap() = snap;
        self.observer.on_changed();
    }

    /// Test-only: mark the box claimed as `run_provisioning_claim` /
    /// `complete_manual_dns_claim` do, so a test can drive
    /// `continue_from_provisioning`'s standard path without a nest to claim.
    /// The un-set state is itself under test (Continue must refuse), so this is
    /// an explicit seam rather than a default.
    pub fn set_claim_completed_for_test(&self, completed: bool) {
        self.mutate(|s| s.claim_completed = completed);
    }

    /// Test-only: inject the verified VPS `locations` as if `verify_vps`
    /// had returned them, marking the config verified and defaulting the
    /// selection to the first (the production `verify_vps` post-state). Lets
    /// the cross-app E2E bridge render + actuate the `vps-location-picker`
    /// without the real cloud-provider HTTP probe (which needs
    /// `set_provider_base_urls`, still web-only).
    /// Mirrors `set_dns_records_for_test`.
    pub fn set_vps_locations_for_test(&self, locations: Vec<fauna_provisioning::vps::VpsLocation>) {
        self.mutate(|s| {
            s.vps.selected_location_id = locations.first().map(|l| l.id.clone());
            s.vps.locations = locations;
            s.vps.verified = true;
        });
    }

    /// Cross-app E2E bridge — `driver.call_machine_method(name, json_arg)`
    /// (design tracked internally). Each
    /// per-app driver routes test-fixture commands here so the same
    /// snapshot-injection JSON works on every platform. iOS and macOS both
    /// delegate from their TestAgent's `call_machine_method` action handler
    /// directly to this method via the UniFFI binding; web has its own
    /// JS-side dispatcher (`__fauna_callMachineMethod` in
    /// `apps/fauna-web/src/lib/onboarding/machine.svelte.ts`) since wasm
    /// bindings don't carry serde for the snapshot types.
    ///
    /// Unknown names are silently ignored to keep the bridge forward-
    /// compatible: tests must observe state via observable getters / UI
    /// assertions, not return values from this method.
    pub fn call_machine_method(&self, name: String, json_arg: String) {
        // Machine-free arms (the nest-identity pin seed/read) live in the free
        // dispatcher so "machine-free" has one definition every app shares.
        if let FreeMethodOutcome::Handled(_) = call_machine_free_method(&name, &json_arg) {
            return;
        }
        match name.as_str() {
            "set_handle_check_snapshot_for_test" => {
                if let Some(snap) =
                    parse_bridge_arg::<HandleCheckSnapshot>(name.as_str(), &json_arg)
                {
                    self.set_handle_check_snapshot_for_test(snap);
                }
            }
            "set_invite_request_snapshot_for_test" => {
                if let Some(snap) =
                    parse_bridge_arg::<InviteRequestSnapshot>(name.as_str(), &json_arg)
                {
                    self.set_invite_request_snapshot_for_test(snap);
                }
            }
            "set_age_claim" => {
                // The store-age test seam (`family-safety.md` § App surface →
                // *Age-band surfaces*, #5): the e2e injects a claim exactly where
                // the platform glue would, so the derived `age_notice` is under
                // test with no store on the box. A JSON `null` clears it.
                if let Some(claim) =
                    parse_bridge_arg::<Option<AgeClaimPlain>>(name.as_str(), &json_arg)
                {
                    self.set_age_claim(claim);
                }
            }
            "set_claim_code_snapshot_for_test" => {
                if let Some(snap) = parse_bridge_arg::<ClaimCodeSnapshot>(name.as_str(), &json_arg)
                {
                    self.set_claim_code_snapshot_for_test(snap);
                }
            }
            "set_nat_mode_snapshot_for_test" => {
                if let Some(snap) = parse_bridge_arg::<NatModeSnapshot>(name.as_str(), &json_arg) {
                    self.set_nat_mode_snapshot_for_test(snap);
                }
            }
            // Stand in for the `Online` claiming substep on a fixture with no
            // claimable nest, so a test can drive the standard path's Continue
            // (which refuses an unclaimed box) without one.
            "set_claim_completed_for_test" => {
                if let Some(done) = parse_bridge_arg::<bool>(name.as_str(), &json_arg) {
                    self.set_claim_completed_for_test(done);
                }
            }
            // (`set_nest_identity_pin_for_test` is now handled by the free
            // dispatcher consulted at the top of this fn — it is machine-free.)
            // Sync `nat_mode_choice` actions (the async submit rides
            // `call_machine_method_async`). The arg is the `NodeMode` serde
            // repr — the lowercase wire form (`"public"` / `"private"`).
            "select_nat_mode" => {
                if let Some(mode) = parse_bridge_arg::<NodeMode>(name.as_str(), &json_arg) {
                    self.select_nat_mode(mode);
                }
            }
            "defer_nat_mode_choice" => {
                let _ = self.defer_nat_mode_choice();
            }
            "set_awaiting_manual_dns_snapshot_for_test" => {
                if let Some(snap) =
                    parse_bridge_arg::<AwaitingManualDnsSnapshot>(name.as_str(), &json_arg)
                {
                    self.set_awaiting_manual_dns_snapshot_for_test(snap);
                }
            }
            "set_step_for_test" => {
                if let Some(step) = parse_bridge_arg::<OnboardingStep>(name.as_str(), &json_arg) {
                    self.set_step_for_test(step);
                }
            }
            "set_handle_check_outcome_for_test" => {
                if let Some(outcome) = parse_bridge_arg::<crate::snapshots::HandleCheckOutcome>(
                    name.as_str(),
                    &json_arg,
                ) {
                    self.set_handle_check_outcome_for_test(outcome);
                }
            }
            // Deliberate bare-string fallback, not a drop: an un-JSON-quoted
            // handle still reaches the setter — excluded from the
            // malformed-payload sweep.
            "set_current_handle" => {
                if let Ok(s) = serde_json::from_str::<String>(&json_arg) {
                    self.set_current_handle(s);
                } else {
                    self.set_current_handle(json_arg);
                }
            }
            // Same bare-string fallback shape as `set_current_handle`
            // above.
            "set_nest_url" => {
                if let Ok(s) = serde_json::from_str::<String>(&json_arg) {
                    self.set_nest_url(s);
                } else {
                    self.set_nest_url(json_arg);
                }
            }
            "reset" => self.reset(),
            "seed_identity" => {
                if let Some(secret) = parse_bridge_arg::<String>(name.as_str(), &json_arg) {
                    self.seed_identity(secret);
                }
            }
            // The deferred-DNS relaunch hydration, driven through the shared
            // bridge so every app's e2e reaches the "Almost ready" surface the
            // same way. Setting `wizard_outcome()` to `AwaitingManualDns` is what
            // makes the client's own wizard-exit handling persist the
            // awaiting-manual-dns slot — so a test arranges the wizard state and
            // the *production* code path does the persisting.
            "seed_awaiting_manual_dns" => {
                #[derive(serde::Deserialize)]
                struct Args {
                    nest_url: String,
                    handle: String,
                    dns_records: Vec<DnsRecordPlain>,
                    claim_code: String,
                }
                if let Some(a) = parse_bridge_arg::<Args>(name.as_str(), &json_arg) {
                    self.seed_awaiting_manual_dns(
                        a.nest_url,
                        a.handle,
                        a.dns_records,
                        a.claim_code,
                    );
                }
            }
            // Total-box-loss recovery branch (box-recovery.md § Recovery UI) —
            // the cross-app e2e driver actuates the entry/method/selection
            // transitions through the same bridge as every other app action.
            "begin_recover_lost_box" => self.begin_recover_lost_box(),
            "seed_identity_for_recovery" => {
                if let Some(secret) = parse_bridge_arg::<String>(name.as_str(), &json_arg) {
                    self.seed_identity_for_recovery(secret);
                }
            }
            "set_recovery_boxes" => {
                if let Some(boxes) = parse_bridge_arg::<Vec<String>>(name.as_str(), &json_arg) {
                    self.set_recovery_boxes(boxes);
                }
            }
            // Deliberate bare-string fallback, not a drop — same shape as
            // `set_current_handle` above.
            "select_recovery_box" => {
                let id = serde_json::from_str::<String>(&json_arg).unwrap_or(json_arg);
                self.select_recovery_box(id);
            }
            "recover_via_cloud" => {
                let _ = self.recover_via_cloud();
            }
            "recover_via_selfhosted" => {
                let _ = self.recover_via_selfhosted();
            }
            "navigate_to_invite_request_for_known_nest" => {
                if let Some((nest_url, handle)) =
                    parse_bridge_arg::<(String, String)>(name.as_str(), &json_arg)
                {
                    self.navigate_to_invite_request_for_known_nest(nest_url, handle);
                }
            }
            "navigate_to_claim_code_for_known_nest" => {
                if let Some((nest_url, handle)) =
                    parse_bridge_arg::<(String, String)>(name.as_str(), &json_arg)
                {
                    self.navigate_to_claim_code_for_known_nest(nest_url, handle);
                }
            }
            "navigate_to_claim_code_for_known_nest_with_code" => {
                if let Some((nest_url, handle, code)) =
                    parse_bridge_arg::<(String, String, String)>(name.as_str(), &json_arg)
                {
                    self.navigate_to_claim_code_for_known_nest_with_code(nest_url, handle, code);
                }
            }
            "set_dns_records_for_test" => {
                if let Some(records) =
                    parse_bridge_arg::<Vec<DnsRecordPlain>>(name.as_str(), &json_arg)
                {
                    self.set_dns_records_for_test(records);
                }
            }
            // The DNS twin of the wasm `setCapturedDnsCredentialForTest` binding
            // (`fauna-wasm-onboarding`): native apps (iOS / macOS / Windows /
            // Android) route through this bridge, so the onboarding→launch glue
            // e2e (`test_onboarding_dns_glue.py`) can inject a pre-verified
            // credential here instead of driving the real DNS verify (which routes
            // through `proxy.fauna.social`, unreachable in the e2e sandbox). Sets
            // `verified=true` + `set_up_later=false` so `captured_dns_credential()`
            // yields `Some` at `LoggedIn`. Pair with a `fake-dns-ok:<zone>` field
            // value + the `FAUNA_DNS_PROVIDER_FAKE` sentinel so the launched
            // client's `PutCredentials.verify()` succeeds offline.
            "set_captured_dns_credential_for_test" => {
                #[derive(serde::Deserialize)]
                struct Captured {
                    provider_id: String,
                    fields: std::collections::HashMap<String, String>,
                }
                if let Some(c) = parse_bridge_arg::<Captured>(name.as_str(), &json_arg) {
                    self.set_dns_state_for_test(|s| {
                        s.selected_provider_id = Some(c.provider_id);
                        s.creds = c
                            .fields
                            .into_iter()
                            .map(|(k, v)| (k, fauna_core::secret::SecretString::from(v)))
                            .collect();
                        s.verified = true;
                        s.set_up_later = false;
                    });
                }
            }
            "set_provisioning_snapshot_for_test" => {
                if let Some(snap) =
                    parse_bridge_arg::<ProvisioningSnapshot>(name.as_str(), &json_arg)
                {
                    self.set_provisioning_snapshot_for_test(snap);
                }
            }
            "set_vps_locations_for_test" => {
                if let Some(locations) = parse_bridge_arg::<Vec<fauna_provisioning::vps::VpsLocation>>(
                    name.as_str(),
                    &json_arg,
                ) {
                    self.set_vps_locations_for_test(locations);
                }
            }
            // E2E: redirect the orchestrator's HTTP provider calls at the
            // `fake_cloud` fixture. The native twin of web's
            // `__fauna_setProviderBaseUrls` — applied in place since native
            // apps keep one page-bound machine (web reconstructs on reload).
            "set_provider_base_urls" => {
                if let Some(urls) =
                    parse_bridge_arg::<HashMap<String, String>>(name.as_str(), &json_arg)
                {
                    self.set_provider_base_urls(urls);
                }
            }
            // The native twin of the wasm `setVpsStateForTest` binding
            // (`fauna-wasm-onboarding/src/lib.rs`): seed a verified VPS config so
            // `run_provisioning_inner` reaches the Server/Online steps without
            // driving the real cloud-provider probe. Mirrors that binding's
            // `VpsSeed` shape exactly so the cross-app `set_vps_state_for_test`
            // JSON works on native too.
            "set_vps_state_for_test" => {
                #[derive(serde::Deserialize)]
                struct VpsSeed {
                    provider_id: String,
                    #[serde(default)]
                    creds: HashMap<String, String>,
                    #[serde(default)]
                    server_types: Vec<fauna_provisioning::vps::ServerTypeInfo>,
                    #[serde(default)]
                    selected_server_type_id: Option<String>,
                    #[serde(default)]
                    locations: Vec<fauna_provisioning::vps::VpsLocation>,
                    #[serde(default)]
                    selected_location_id: Option<String>,
                }
                if let Some(s) = parse_bridge_arg::<VpsSeed>(name.as_str(), &json_arg) {
                    self.set_vps_state_for_test(|st| {
                        st.selected_provider_id = Some(s.provider_id);
                        st.creds = s
                            .creds
                            .into_iter()
                            .map(|(k, v)| (k, SecretString::from(v)))
                            .collect();
                        st.server_types = s.server_types;
                        st.selected_server_type_id = s.selected_server_type_id;
                        st.locations = s.locations;
                        st.selected_location_id = s.selected_location_id;
                        st.verified = true;
                    });
                }
            }
            // The native twin of the wasm `setDnsAvailabilityForTest` binding:
            // seed `dns.buy_domain` + `dns.current_availability` so
            // `provider_status()` reaches `UnregisteredBuyable` without driving
            // the real registrar-verify probe — the domain-line twin of
            // `set_vps_state_for_test`, feeding `bill_of_materials()`'s optional
            // domain-registration line (onboarding.md §6).
            "set_dns_availability_for_test" => {
                #[derive(serde::Deserialize)]
                struct DnsAvailabilitySeed {
                    #[serde(default = "default_test_dns_provider_id")]
                    provider_id: String,
                    buy_domain: bool,
                    price_cents: u64,
                    #[serde(default)]
                    currency: Option<String>,
                }
                fn default_test_dns_provider_id() -> String {
                    "cloudflare".to_string()
                }
                if let Some(s) = parse_bridge_arg::<DnsAvailabilitySeed>(name.as_str(), &json_arg) {
                    self.set_dns_availability_for_test(
                        s.provider_id,
                        s.buy_domain,
                        s.price_cents,
                        s.currency,
                    );
                }
            }
            // Bucket-1 IPC injectors for the live-provision e2e (no human config
            // surface): pin the nest image tag / attach sweepable labels for the
            // next real provisioning run. `set_provision_image_tag`'s arg is the
            // bare tag string (JSON-quoted or not); `set_provision_labels`' arg is
            // a JSON array of `[key, value]` pairs (e.g. `[["fauna-e2e","1"]]`).
            // Pin the claim code for the next run — the tier_3 journey that
            // claims a REAL nest, which booted with the harness's code already
            // on disk. Test-only by name and by contract; see the setter.
            //
            // The next two arms are deliberate bare-string fallbacks, not
            // drops — same shape as `set_current_handle`
            // above.
            "set_provision_claim_code_for_test" => {
                let code = serde_json::from_str::<String>(&json_arg).unwrap_or(json_arg);
                self.set_provision_claim_code_for_test(code);
            }
            "set_provision_image_tag" => {
                let tag = serde_json::from_str::<String>(&json_arg).unwrap_or(json_arg);
                self.set_provision_image_tag(tag);
            }
            "set_provision_labels" => {
                if let Some(labels) =
                    parse_bridge_arg::<Vec<(String, String)>>(name.as_str(), &json_arg)
                {
                    self.set_provision_labels(labels);
                }
            }
            // Real (non-fixture) DNS-stage methods driven by the vps-config e2e
            // helper `actions/onboarding.py::go_to_vps_config_with_dns_provider`.
            // Web reaches these through its wasm-reflecting JS dispatcher
            // (`__fauna_callMachineMethod`); native apps route here, so they
            // must be in this match too. All four are **sync** — the async
            // `verify_dns` can't go through this sync dispatcher and is driven
            // to completion by the per-app bridge instead (linux:
            // `async_helper::block_on_tokio` in the `"machine"` handler).
            "toggle_same_provider_for_vps" => {
                if let Some(on) = parse_bridge_arg::<bool>(name.as_str(), &json_arg) {
                    self.toggle_same_provider_for_vps(on);
                }
            }
            // Deliberate bare-string fallback, not a drop — same shape as
            // `set_current_handle` above.
            "select_dns_provider" => {
                if let Ok(id) = serde_json::from_str::<String>(&json_arg) {
                    self.select_dns_provider(id);
                } else {
                    self.select_dns_provider(json_arg);
                }
            }
            "set_dns_cred" => {
                // `(field_id, value)` arrives as a JSON 2-array, mirroring the
                // helper's `json.dumps(["api-token", "MOCK"])`.
                if let Some((field, value)) =
                    parse_bridge_arg::<(String, String)>(name.as_str(), &json_arg)
                {
                    self.set_dns_cred(field, value);
                }
            }
            "continue_from_dns" => {
                let _ = self.continue_from_dns();
            }
            _ => {}
        }
    }

    /// Value-returning variant of [`Self::call_machine_method`] for the native
    /// E2E bridge (linux/apple/…). Setter names delegate to
    /// `call_machine_method` and return `None`; **reader** names return the
    /// method's JSON-serialized result so the driver's `call_machine_method`
    /// can hand it back to the Python test — the parity with web's
    /// `__fauna_callMachineMethod`, which already returns getter results by
    /// reflecting over the wasm surface.
    ///
    /// Kept separate from `call_machine_method` (which stays `-> ()`) so the
    /// existing UniFFI callers that ignore the return (apple/android/windows)
    /// are unaffected; they migrate to this method when they wire reader
    /// returns through their own TestAgent.
    pub fn call_machine_method_with_result(
        &self,
        name: String,
        json_arg: String,
    ) -> Option<String> {
        // Machine-free arms (the nest-identity pin seed/read) first — one shared
        // definition of "machine-free", so a post-auth caller with no machine can
        // reach the same seed/read this dispatcher exposes.
        if let FreeMethodOutcome::Handled(v) = call_machine_free_method(&name, &json_arg) {
            return v;
        }
        match name.as_str() {
            // Reader: the live provisioning snapshot, so the test can poll the
            // orchestrator's progress (e.g. "Online step is Running").
            "provisioning_snapshot" => serde_json::to_string(&self.provisioning_snapshot()).ok(),
            // Reader: confirm a `set_provider_base_urls` override actually
            // applied (the test asserts this before driving a real run, so the
            // orchestrator can't escape to the live internet). `json_arg` is the
            // JSON-encoded key string (e.g. `"dns"`); accept a bare string too.
            "provider_base_url" => {
                let key = serde_json::from_str::<String>(&json_arg).unwrap_or(json_arg);
                Some(
                    serde_json::to_string(&self.provider_base_url(key))
                        .unwrap_or_else(|_| "null".to_string()),
                )
            }
            // (`nest_identity_pin_for_test` reader is now handled by the free
            // dispatcher consulted at the top of this fn — it is machine-free.)
            // Everything else is a setter/command — run it, return no value.
            _ => {
                self.call_machine_method(name, json_arg);
                None
            }
        }
    }
}

/// What [`OnboardingMachine::submit_recovery_entry`] produced.
///
/// One variant per thing the user can do next, which is what makes it narrower
/// than the transport's own [`crate::nest_api::RestoreSeedError`]. Each maps to
/// an `onboarding.recovery_entry.*` i18n key the **client** resolves — the
/// machine holds no string table, the same division the handle-check's
/// `LocalizedText` snapshots use.
/// The `uniffi::Enum` derive is **mandatory, not optional**: this is the return
/// type of `submit_recovery_entry`, which lives in the `#[uniffi::export]`ed
/// `impl OnboardingMachine` block, so without it the whole `uniffi`-feature
/// build fails (`LowerReturn`/`Lift`/`TypeId` unsatisfied) and takes the
/// mail-bridge and apple FFI gates with it. Verify with
/// `cargo check -p fauna-onboarding-machine --features uniffi` — the
/// default-feature build says nothing about it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RecoveryEntryOutcome {
    /// The seed is restored and the wizard is on `handle_entry`, with the
    /// account carried into the handle field.
    Restored,
    /// [`Self::Restored`] — the account **is** back and the wizard advanced
    /// identically — but the blob's predecessor section was present and did not
    /// open, so a corpus still sealed under a predecessor identity is now
    /// unopenable (`identity-succession.md` § Seed escrow). Key
    /// `restored_predecessors_lost` (arg `reason`).
    ///
    /// A separate variant rather than a flag on `Restored` because a client
    /// must not be able to render the success path without deciding what to say
    /// here: this is the one place a *successful* recovery still lost
    /// user-irrecoverable data, and silence is the failure mode that matters.
    RestoredPredecessorsLost { reason: String },
    /// The phrase is not a recovery kit — refused by the local parse, so
    /// nothing was sent. Key `invalid_kit`.
    InvalidKit,
    /// Nothing named the account: the payload carried no handle and the
    /// account field was empty. Key `account_needed`.
    AccountNeeded,
    /// The account was named but is not `user@domain`, so there is no domain to
    /// find a nest at. Key `account_malformed`.
    AccountMalformed,
    /// The nest was reached and knows no such account. Key `account_unknown`
    /// (arg `account`).
    AccountUnknown { account: String },
    /// No escrow blob rests for this account — the ratified honest signal, not
    /// a fault. Key `no_escrow`.
    NoEscrow,
    /// The identity was succeeded. The **client** routes this to
    /// [`OnboardingMachine::begin_import_identity_with_reason`] with the
    /// `superseded` string, uniform with the launch flow's refusal; the
    /// successor is deliberately not carried, because it arrives unverified.
    Superseded,
    /// The nest declined the kit — replaced, retired, or another account's.
    /// Key `refused` (arg `reason`).
    Refused { reason: String },
    /// The nest could not be reached or resolved; a retry is worth offering.
    /// Key `unreachable` (arg `reason`).
    Unreachable { reason: String },
}

impl RecoveryEntryOutcome {
    /// The message a client renders on `error-message` for this outcome — the
    /// `onboarding.recovery_entry.*` key each variant's doc names, with its
    /// argument — so all seven apps say the same thing and none re-derives the
    /// table. `None` for the two that route rather than speak: `Restored` (the
    /// wizard advanced, nothing to say) and `Superseded` (the client sends the
    /// user to [`OnboardingMachine::begin_import_identity_with_reason`]).
    ///
    /// ⚠ `RestoredPredecessorsLost` is a success that still speaks: the account
    /// is back, and the message is what stops that success implying a complete
    /// one.
    pub fn message(&self) -> Option<LocalizedText> {
        const P: &str = "onboarding.recovery_entry.";
        let key = |k: &str| format!("{P}{k}");
        Some(match self {
            Self::Restored | Self::Superseded => return None,
            Self::RestoredPredecessorsLost { reason } => {
                LocalizedText::key_arg(key("restored_predecessors_lost"), "reason", reason)
            }
            Self::InvalidKit => LocalizedText::key(key("invalid_kit")),
            Self::AccountNeeded => LocalizedText::key(key("account_needed")),
            Self::AccountMalformed => LocalizedText::key(key("account_malformed")),
            Self::AccountUnknown { account } => {
                LocalizedText::key_arg(key("account_unknown"), "account", account)
            }
            Self::NoEscrow => LocalizedText::key(key("no_escrow")),
            Self::Refused { reason } => LocalizedText::key_arg(key("refused"), "reason", reason),
            Self::Unreachable { reason } => {
                LocalizedText::key_arg(key("unreachable"), "reason", reason)
            }
        })
    }
}

/// [`RecoveryEntryOutcome::message`] as a free function — the UniFFI door the
/// FFI apps (android, apple, windows) render the restore's answer through, so
/// they read the same table tui, linux and web do instead of re-deriving it.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn recovery_entry_outcome_message(outcome: RecoveryEntryOutcome) -> Option<LocalizedText> {
    outcome.message()
}

/// Outcome of [`call_machine_free_method`]: whether the E2E-bridge name was a
/// **machine-free** method (and its optional reader value), or genuinely needs a
/// live [`OnboardingMachine`].
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub enum FreeMethodOutcome {
    /// Handled without a machine. The inner value is a reader's JSON-serialized
    /// result (`None` for a setter/command).
    Handled(Option<String>),
    /// Not a machine-free method — the caller must route it to a live machine.
    NeedsMachine,
}

/// The machine-free arms of the cross-app E2E bridge — the ones that touch
/// only **process-global** state and so need no [`OnboardingMachine`] at all.
///
/// Today that is exactly the nest-identity TOFU pin seed
/// (`set_nest_identity_pin_for_test`) and its reader (`nest_identity_pin_for_test`),
/// which write/read the process-global pin store
/// (`fauna_anon_client::trust` on native, `LocalStoragePinStore` on web).
///
/// **Why a free function and not just two match arms.** A client must be able to
/// drive these on a *live authenticated session*, where no onboarding machine is
/// registered — the post-auth "nest identity changed" e2e re-seeds the pin after
/// login (`security.md` § Post-auth surfacing). Routing them through a method on
/// `OnboardingMachine` forces every app's agent to hold a machine it does not
/// have post-auth. Lifting them to a free dispatcher every app can call
/// with-or-without a machine (priority #2) is the shared fix. The two dispatchers
/// above delegate here, so "machine-free" has exactly one definition, and a
/// client that hits its no-machine early-return tries here first.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub fn call_machine_free_method(name: &str, json_arg: &str) -> FreeMethodOutcome {
    match name {
        // Seed a TOFU nest-identity pin, so the next silent challenge meets a pin
        // the nest cannot prove and routes to `LaunchPhase::IdentityChanged`
        // instead of auto-entering. The pin store is process-global and installed
        // per-app, so this ONE arm seeds `DiskPinStore` on every native app
        // and `LocalStoragePinStore` on web — no per-app harness code, and the
        // test never learns either backend's shape. The arg carries the nest URL
        // and each side derives the key exactly as its production path does
        // (native: the URL's authority; web: the origin verbatim), so the test
        // never has to know origin-vs-host.
        "set_nest_identity_pin_for_test" => {
            if let Some(arg) = parse_bridge_arg::<NestIdentityPinArg>(name, json_arg) {
                match fauna_core::hex32::decode(&arg.actor_id) {
                    Ok(actor_id) => {
                        #[cfg(not(target_arch = "wasm32"))]
                        fauna_anon_client::trust::pin_identity_for_test(&arg.nest_url, actor_id);
                        #[cfg(target_arch = "wasm32")]
                        {
                            use fauna_client_core::nest_trust::NestIdentityPinStore as _;
                            fauna_client_core::nest_trust::LocalStoragePinStore
                                .set(&arg.nest_url, actor_id);
                        }
                    }
                    // A malformed actor_id hex is a second silent-drop point
                    // parse_bridge_arg's own JSON-shape check cannot
                    // cover.
                    Err(error) => {
                        tracing::warn!(
                            method = name,
                            %error,
                            "e2e bridge: malformed actor_id hex in set_nest_identity_pin_for_test"
                        );
                    }
                }
            }
            FreeMethodOutcome::Handled(None)
        }
        // Reader: the TOFU pin the installed store currently holds for a nest, as a
        // hex string (`null` when none). The read half of the seed above — it lets
        // the cross-app journey assert "the trust button FORGOT the pin" through
        // the bridge instead of reaching into a per-app backend.
        "nest_identity_pin_for_test" => {
            let Ok(arg) = serde_json::from_str::<NestIdentityPinQuery>(json_arg) else {
                return FreeMethodOutcome::Handled(Some("null".to_string()));
            };
            #[cfg(not(target_arch = "wasm32"))]
            let pin = fauna_anon_client::trust::pinned_identity_for_test(&arg.nest_url);
            #[cfg(target_arch = "wasm32")]
            let pin = {
                use fauna_client_core::nest_trust::NestIdentityPinStore as _;
                fauna_client_core::nest_trust::LocalStoragePinStore.get(&arg.nest_url)
            };
            FreeMethodOutcome::Handled(serde_json::to_string(&pin.map(hex::encode)).ok())
        }
        // Seed a CHAIN-ACCEPTED re-pin (`NestIdentityPinStore::
        // set_rotation_accepted`), not an ordinary TOFU one — the
        // precondition a test needs before it can assert the guard's own
        // property:
        // a same-root re-seed must not erase an existing `rotation_seq`.
        // No production path does this outside a real verified rotation
        // chain.
        "set_nest_identity_pin_rotation_accepted_for_test" => {
            if let Some(arg) = parse_bridge_arg::<NestIdentityPinRotationArg>(name, json_arg) {
                match fauna_core::hex32::decode(&arg.actor_id) {
                    Ok(actor_id) => {
                        #[cfg(not(target_arch = "wasm32"))]
                        fauna_anon_client::trust::set_rotation_accepted_for_test(
                            &arg.nest_url,
                            actor_id,
                            arg.seq,
                        );
                        #[cfg(target_arch = "wasm32")]
                        {
                            use fauna_client_core::nest_trust::NestIdentityPinStore as _;
                            fauna_client_core::nest_trust::LocalStoragePinStore
                                .set_rotation_accepted(&arg.nest_url, actor_id, arg.seq);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            method = name,
                            %error,
                            "e2e bridge: malformed actor_id hex in set_nest_identity_pin_rotation_accepted_for_test"
                        );
                    }
                }
            }
            FreeMethodOutcome::Handled(None)
        }
        // Reader: the chain-accepted `rotation_seq` the installed store holds
        // for a nest, if any (`null` when none) — the property the guard
        // protects, and otherwise unobservable through the bridge
        // (`nest_identity_pin_for_test` above answers only the pinned actor
        // id).
        "nest_identity_pin_rotation_seq_for_test" => {
            let Ok(arg) = serde_json::from_str::<NestIdentityPinQuery>(json_arg) else {
                return FreeMethodOutcome::Handled(Some("null".to_string()));
            };
            #[cfg(not(target_arch = "wasm32"))]
            let seq = fauna_anon_client::trust::rotation_seq_for_test(&arg.nest_url);
            #[cfg(target_arch = "wasm32")]
            let seq = {
                use fauna_client_core::nest_trust::NestIdentityPinStore as _;
                fauna_client_core::nest_trust::LocalStoragePinStore.rotation_seq(&arg.nest_url)
            };
            FreeMethodOutcome::Handled(serde_json::to_string(&seq).ok())
        }
        _ => FreeMethodOutcome::NeedsMachine,
    }
}

/// Arg of the E2E bridge's `set_nest_identity_pin_for_test` — the nest whose pin
/// to seed, plus the 32-byte `nest_actor_id` to pin for it, hex-encoded. A pin no
/// nest can prove (e.g. `"ab" * 32`) is what drives the launch path to
/// `LaunchPhase::IdentityChanged`.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(serde::Deserialize)]
struct NestIdentityPinArg {
    nest_url: String,
    actor_id: String,
}

/// Arg of the E2E bridge's `set_nest_identity_pin_rotation_accepted_for_test`
/// — same shape as [`NestIdentityPinArg`] plus the chain-accepted
/// `seq`.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(serde::Deserialize)]
struct NestIdentityPinRotationArg {
    nest_url: String,
    actor_id: String,
    seq: u64,
}

/// Arg of the E2E bridge's `nest_identity_pin_for_test` reader.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(serde::Deserialize)]
struct NestIdentityPinQuery {
    nest_url: String,
}

/// The async half of the E2E bridge, for clients that drive it from an async
/// runtime (cli's tokio main loop; linux's worker runtime via `block_on_tokio`).
///
/// **Now `uniffi::export`ed** (2026-08-29). It previously was not, on the
/// rationale that "cli and linux consume this crate as an ordinary Rust
/// dependency, and exporting would flip the UniFFI checksum for every other
/// app's bindings to buy nothing." The second half of that stopped being true:
/// the UniFFI apps need it. Measured on macOS, the live Hetzner provisioning
/// drive against the macOS app never started (`overall: 'Idle'`, every step
/// `Pending`) because `verify_dns` and `wizard_submit_claim_code` fell into the
/// SYNC dispatcher's silent `_` arm — the exact failure this type's own
/// docstring warns about. Windows sits on the identical gap
/// (`FaunaApp/Testing/TestAgent.cs` calls `CallMachineMethodWithResult`).
///
/// Exporting keeps the async name table in shared Rust for all of them, which
/// is what tui's bridge comment argues for, instead of a third hand-written
/// copy per UniFFI app. The export is gated on `test-helpers` exactly like its
/// sync twin's block, so release builds — and their checksums — are unchanged.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(
    all(feature = "uniffi", feature = "test-helpers"),
    fauna_uniffi_async::export
)]
impl OnboardingMachine {
    /// Async twin of [`Self::call_machine_method_with_result`].
    ///
    /// The sync dispatchers cannot `.await`, so every async machine method
    /// falls through their `_` arm and is **silently ignored** — a
    /// `verify_dns` over the bridge would ack green having done nothing. That
    /// is the bug this method closes: the async names below run to completion
    /// here, so the client's bridge ack lands only once the effect is in the
    /// snapshot, which is what `onboarding.md` § E2E bridge contract requires
    /// ("Async methods block until complete… so a test can drive `verify_dns`
    /// then immediately `continue_from_dns` without a poll"). Every other name
    /// delegates to the sync dispatcher, so there is one name table, not two.
    ///
    /// **Fire-and-forget orchestration is deliberately NOT here.**
    /// `start_provisioning` / `retry_provisioning` spawn a task that outlives
    /// the call (provisioning takes minutes; the bridge must return at once and
    /// let the driver poll `provisioning_snapshot`). Whether that spawn is
    /// `tokio::spawn` on an ambient runtime (CLI), a dedicated worker thread
    /// whose runtime must outlive the task (linux — its `block_on_tokio` builds
    /// a current-thread runtime and **drops it on return**, which would cancel
    /// anything spawned inside), or `spawn_local` (web) is genuinely
    /// platform-divergent runtime ownership, so each app keeps that one arm.
    /// Awaiting them here instead would break the return-immediately contract.
    pub async fn call_machine_method_async(
        &self,
        name: String,
        json_arg: String,
    ) -> Option<String> {
        /// The bridge sends a JSON-encoded string; tolerate a bare one too
        /// (the same leniency `call_machine_method` applies to `set_*`).
        fn arg(json_arg: &str) -> String {
            serde_json::from_str::<String>(json_arg).unwrap_or_else(|_| json_arg.to_string())
        }
        /// An `OnboardingStep`-returning method is a reader as far as the
        /// bridge is concerned: the driver reads the routed-to step back.
        fn step(step: OnboardingStep) -> Option<String> {
            serde_json::to_string(&step).ok()
        }

        match name.as_str() {
            // Commands: no reader value, but the ack must wait for the effect.
            "verify_dns" => {
                let _ = self.verify_dns().await;
                None
            }
            "verify_vps" => {
                let _ = self.verify_vps().await;
                None
            }
            "continue_from_vps" => {
                let _ = self.continue_from_vps().await;
                None
            }
            "start_handle_check" => {
                self.start_handle_check(arg(&json_arg)).await;
                None
            }
            "verify_oob_invite_code" => {
                self.verify_oob_invite_code(arg(&json_arg)).await;
                None
            }
            // Reader: the glue's own mint (`fauna.account.age_nonce` against the
            // wizard's nest), so the e2e reaches the verified notice exactly the
            // way the store-age round does — through the send guard, never past
            // it. `{"Ok": AgeNoncePlain}` | `{"Err": "<detail>"}`.
            "request_age_nonce" => {
                serde_json::to_string(&self.request_age_nonce().await.map_err(|e| e.to_string()))
                    .ok()
            }
            // Step-returning: the driver reads the routed-to step back.
            "submit_handle_check_continue" => step(self.submit_handle_check_continue().await),
            "wizard_submit_claim_code" => step(self.wizard_submit_claim_code(arg(&json_arg)).await),
            "wizard_submit_invite_request" => step(self.wizard_submit_invite_request().await),
            "recheck_invite_status" => step(self.recheck_invite_status().await),
            "redeem_invite" => step(self.redeem_invite().await),
            "recheck_manual_dns" => step(self.recheck_manual_dns().await),
            "submit_nat_mode_choice" => step(self.submit_nat_mode_choice().await),
            // Sync setters, readers, and the `set_*_for_test` fixtures.
            _ => self.call_machine_method_with_result(name, json_arg),
        }
    }
}

#[derive(Debug)]
enum NextStep {
    DnsPostInstructions,
    Provisioned { url: String },
}

fn stash_provisioning_result(
    machine: Arc<OnboardingMachine>,
    result: Result<NextStep, OnboardingError>,
) {
    use fauna_provisioning::progress::OverallStatus;

    machine.mutate(|s| {
        s.is_loading = false;
        match &result {
            Ok(NextStep::DnsPostInstructions) => {
                let domain = s.handle_domain().unwrap_or_default().to_string();
                s.nest_url = format!("https://{domain}");
                s.error_message = None;
            }
            Ok(NextStep::Provisioned { url }) => {
                s.nest_url = url.clone();
                s.error_message = None;
            }
            Err(e) => {
                s.set_error(e.to_string());
            }
        }
    });
    if result.is_ok() {
        // The box is reachable on its captured static IP the instant
        // provisioning succeeds, but its DNS record may not have propagated
        // yet (Hetzner beta DNS ~30 min) — point `WsNestApi` at the IP for
        // this domain so any pre-identity call for the same domain doesn't
        // block on DNS. All three provisioning paths (deferred-DNS, buy-domain,
        // standard) stash `ProvisionResultPlain` into `machine.provisioning`
        // via `set_run_succeeded` before `run_provisioning_inner` returns.
        //
        // The standard path has already installed this, from the same helper,
        // before its in-run claim — which needs the override rather than
        // merely benefiting from it. Idempotent; this call is what covers the
        // paths that never claim.
        machine.install_reach_override();
    }
    // If run_provisioning_inner returned Err before the orchestrator had a
    // chance to call `set_failed` (e.g. missing handle or server type), the
    // provisioning snapshot's overall status will still be Idle or Running.
    // Ensure the client can observe a terminal failure state in that case.
    if let Err(e) = &result {
        let mut guard = machine.provisioning.lock().unwrap();
        if !matches!(
            guard.overall,
            OverallStatus::Failed | OverallStatus::Cancelled
        ) {
            guard.overall = OverallStatus::Failed;
            guard.final_error = Some(e.to_string());
        }
    }
    // Fire a final observer notification so clients reading
    // `provisioning_snapshot()` on ticks always see the terminal state.
    // On the Ok paths this is a harmless extra tick; on the Err path it is
    // the only tick that reflects the Failed snapshot written above
    // (the earlier `machine.mutate(...)` fired before the snapshot update).
    machine.observer.on_changed();
}

/// Drives the orchestrator end-to-end for all three provisioning paths
/// (deferred-DNS, buy-domain, standard). `start_provisioning` spawns
/// this on the appropriate runtime; the result feeds
/// `stash_provisioning_result`.
impl OnboardingMachine {
    /// Resolve the `(domain, deployment_seed_hex)` the provisioning drive
    /// installs — the one branch point between first-provision and recovery-mode
    /// re-provision (box-recovery.md § Recovery UI (step 4), leg 3).
    ///
    /// **Normal provisioning** (first box): mint a fresh seed
    /// (`generate_deployment_seed` — the box's client-generated identity origin,
    /// custodied later via the claim hand-off) and use the admin's **handle**
    /// domain.
    ///
    /// **Recovery-mode re-provision** (`recovery_intent`): resolve the
    /// **selected** lost box's *custodied* seed + its **own** domain from the
    /// admin's custody on the reachable surviving nest (via
    /// `recovery_config_reader`), so the rebuilt box re-presents the same
    /// `nest_actor_id` (every TOFU-pinned client reconnects) and the A/AAAA
    /// re-point targets the box's domain, not the admin's handle domain. The seed
    /// is resolved **in Rust** and hex-rendered only at the cloud-init boundary
    /// below — it never crosses into JS.
    ///
    /// Extracted from `run_provisioning_inner` so this branch is directly
    /// unit-testable (fake reader → known seed/domain). ⚠ The extraction note
    /// used to add "because the downstream does real cloud IO with no fakeable
    /// seam" — that was **wrong**: `dispatch::{vps,dns}_provider` take an
    /// `override_base_url` and the machine reads it per call via
    /// `provider_base_url`, so the whole drive fakes cleanly. Testing this
    /// resolution alone is *not* sufficient — see
    /// `recovery_drive_sends_the_custodied_seed_to_the_vps_create_call`, which
    /// covers the links between here and the provider's create call.
    /// Track 2 — first-contact trust (security.md § Transport trust, Axis 2;
    /// design tracked internally): the
    /// client knows the box's identity a priori (it is injecting the seed), so
    /// hold the derived PUBLIC key as the pre-resolved identity root for this
    /// domain. `WsNestApi` graduates every pre-identity connection to `domain`
    /// against it — the box must present exactly this identity from the very
    /// first connect (a mismatch on a box we just provisioned is MITM/bug, not
    /// benign). **Mint path only** — a fresh, just-generated seed, so a
    /// malformed hex can only mean the local generator misbehaved and writes
    /// nothing (the box would mint its own identity anyway —
    /// `deployment_key::decode_deployment_seed` ignores it nest-side the same
    /// way). The recovery path does NOT call this: it holds the selected box's
    /// own identity directly and refuses the run on a derive mismatch instead
    /// (`run_provisioning_inner`'s trust-root selection) — a corrupt custodied
    /// seed there must stop the run, not silently pin nothing. Public key
    /// only — the seed itself stays under separate handling.
    fn hold_first_contact_root(&self, domain: &str, deployment_seed_hex: &str) {
        if let Some(expected) = nest_actor_id_from_seed_hex(deployment_seed_hex) {
            self.hold_first_contact_identity(domain, expected);
        }
    }

    /// The public-identity sibling of [`Self::hold_first_contact_root`] — the
    /// self-hosted arm of Track 2 (security.md § Transport trust, Axis 2). The
    /// VPS flow derives the expected identity from the seed it injected; here
    /// the admin read it off the nest's console (the `fauna://claim` URI
    /// printed beside the claim code) and pasted it with the code, so the
    /// console is the out-of-band channel. Same hold either way: `WsNestApi`
    /// graduates every pre-identity connection to `host` against it — claim,
    /// storage-mode, mail-enable each self-verify, and a mismatch hard-fails
    /// (an on-path interceptor cannot forge the identity's signature over the
    /// SPKI the client saw, so it can no longer harvest the claim code).
    fn hold_first_contact_identity(&self, host: &str, expected: [u8; 32]) {
        *self
            .nest_expected_identity
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some((host.to_string(), expected));
    }

    async fn resolve_deployment_seed_and_domain(
        &self,
        snapshot: &State,
    ) -> Result<(String, String), OnboardingError> {
        if !snapshot.recovery_intent {
            let domain = snapshot
                .handle_domain()
                .ok_or_else(|| OnboardingError::InvalidTransition {
                    from: "vps_config".into(),
                    reason: "handle has no domain part".into(),
                })?
                .to_string();
            return Ok((domain, fauna_provisioning::generate_deployment_seed()));
        }

        // Recovery mode: read the custodied seed + domain for the selected box.
        let nest_actor_id_hex = snapshot.recovery_selected_nest_id.clone().ok_or_else(|| {
            OnboardingError::InvalidTransition {
                from: "vps_config".into(),
                reason: "recovery-mode provisioning with no box selected".into(),
            }
        })?;
        let owner_secret =
            snapshot
                .imported_secret
                .clone()
                .ok_or_else(|| OnboardingError::InvalidTransition {
                    from: "vps_config".into(),
                    reason: "recovery-mode provisioning with no imported identity".into(),
                })?;
        // No nest URL is not a refusal: this device's own store may hold the
        // box (the surviving device whose only nest is the dead box), and the
        // reader joins it with a cold read whenever a nest URL is given.
        let nest_url = Some(snapshot.nest_url.as_str()).filter(|u| !u.is_empty());

        let reader = self.recovery_config_reader.read().unwrap().clone();
        let input = reader
            .resolve(nest_url, &owner_secret, &nest_actor_id_hex)
            .await
            .map_err(|e| OnboardingError::ProvisioningFailed {
                step: "recovery-config".into(),
                detail: e.to_string(),
            })?;
        let domain = input
            .domain
            .ok_or_else(|| OnboardingError::InvalidTransition {
                from: "vps_config".into(),
                reason: "the recovered box has no domain — recover via self-hosted instead".into(),
            })?;
        Ok((domain, hex::encode(input.deployment_seed)))
    }
}

async fn run_provisioning_inner(
    machine: Arc<OnboardingMachine>,
) -> Result<NextStep, OnboardingError> {
    use fauna_provisioning::dispatch;

    let snapshot = machine.snapshot();
    let http = machine.http.clone();
    // The orchestrator's Step-4 nest liveness poll is reached by the box's
    // captured static IP via a `probe_for_ip` closure built per provisioning
    // path below (a temporary DNS override so the poll succeeds before public
    // DNS propagates) — see `nest_probe_client_resolving`. The strict
    // `http` client above stays for the cloud-provider API calls.

    // E2E-only overrides: when the test fixture passes a base-URL map, redirect
    // the orchestrator's outbound provider HTTP calls (VPS/DNS) and the final
    // `nest_url` to the fake-cloud server. Production callers pass `None` and
    // these accessors return `None`, leaving behaviour unchanged.
    let vps_base = machine.provider_base_url("vps".into());
    let dns_base = machine.provider_base_url("dns".into());
    let nest_base = machine.provider_base_url("nest".into());

    // Box-recovery (box-recovery.md § Recovery UI (step 4), leg 3): resolve the
    // `(domain, deployment_seed)` this box provisions with. Normal provisioning
    // mints a fresh seed + uses the admin's handle domain; recovery-mode
    // re-provision resolves the *selected lost box's* custodied seed + its own
    // domain from the admin's custody on the reachable surviving nest, so the
    // rebuilt box re-presents the same `nest_actor_id`. The seed stays
    // Rust-internal — hex only crosses at the cloud-init
    // boundary below. See `resolve_deployment_seed_and_domain`.
    let (domain, deployment_seed) = machine
        .resolve_deployment_seed_and_domain(&snapshot)
        .await?;

    let st = snapshot
        .vps
        .server_types
        .iter()
        .find(|st| Some(&st.id) == snapshot.vps.selected_server_type_id.as_ref())
        .cloned()
        .ok_or_else(|| OnboardingError::InvalidTransition {
            from: "vps_config".into(),
            reason: "no server type selected".into(),
        })?;
    // **Custody precedes dispatch** (`docs/goal/behavior/onboarding.md` § 6 *The
    // pending-provision slot*). The claim code exists only in this client until
    // the box is claimed, so minting it and *then* dying — anywhere between here
    // and the claim — would orphan a box nobody can claim and a bill nobody can
    // stop from the app, the shape `nest/common.md` § Client-state
    // recoverability forbids. So the slot is written first and the code comes
    // back out of the write: `mint_and_persist_pending_provision` cannot hand out
    // a code it has not already read back from the store, which is what makes the
    // crash-unsafe ordering unrepresentable rather than merely discouraged.
    //
    // `reach_ipv4` is completed by `on_server_ready` below, and on the deferred
    // path the records-to-paste arrive later at the `AwaitingManualDns` exit —
    // both *complete* this row rather than replacing it.
    //
    // An app that wired no store falls back to a bare mint: it provisions exactly
    // as it did before this row, minus the crash resume. That is a real
    // regression, so it is a pinned test rather than a runtime surprise (see
    // `new_with_persistence`).
    //
    // **A re-run resumes the box an earlier run built — it does not mint over
    // it.** The orchestrator's Server step is a name-only pre-flight: a run of
    // the same domain finds the box the last run created and skips
    // `create_server`, but that box boots with the LAST run's cloud-init — its
    // claim code and its injected identity. So for the `(nest_url, handle)` this
    // machine already holds a pending-provision row for (`pending_provision_row`:
    // a Retry after a failed Online, a "start over" onto the same domain, a
    // relaunch's re-seed), this run reuses that row's code and holds that row's
    // identity as the first-contact root; only a run with no such row mints. The
    // fresh seed in `deployment_seed` is still what cloud-init would carry, and
    // `note_server_ready` switches the held root (and the slot) to it iff the
    // Server step actually created a box. Before this, every Retry against an
    // existing box hard-failed deterministically at first contact with
    // `nest_actor_id is not the expected nest` — the right refusal of a box that
    // does not carry the identity you expect, aimed at the wrong box.
    let slot_nest_url = nest_base
        .clone()
        .unwrap_or_else(|| format!("https://{domain}"));
    let slot_handle = machine.current_handle();
    let slot_store = machine
        .pending_provision
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let fresh_identity = nest_actor_id_from_seed_hex(&deployment_seed).map(hex::encode);
    let retained = machine.retained_pending_provision(&slot_nest_url, &slot_handle);
    // What this machine knew of the box BEFORE this run — the identity a box
    // the Server step *finds* can be expected to boot with. Captured now, since
    // the row retained below carries this run's planned identity instead.
    let prior_identity = retained.as_ref().and_then(|row| row.nest_actor_id.clone());
    // Track 2 — first-contact trust: hold the box's identity as the pre-resolved
    // root for this domain's pre-identity connections (`hold_first_contact_root`)
    // — the retained one when resuming, else the fresh seed's on the mint path.
    // On the recovery path the client already holds the authoritative expected
    // identity (`recovery_selected_nest_id` — the very box being recovered), so
    // hold that directly rather than re-deriving it from the custodied seed, and
    // refuse the run when the custodied seed does not derive to it: a mismatch
    // means the custodied seed entry is corrupt, and a corrupt custody entry
    // must stop the run (security.md § Transport trust, Axis 2 — the
    // *Client-provisioned box* row's "no TOFU window" does not hold if a
    // corrupt custodied seed is let through to `hold_first_contact_root`'s
    // mint-path malformed-seed tolerance, which holds nothing and silently
    // downgrades to TOFU-on-host).
    match retained
        .as_ref()
        .and_then(|row| row.nest_actor_id.as_deref())
        .and_then(decode_nest_actor_id_hex)
    {
        Some(id) => machine.hold_first_contact_identity(&domain, id),
        None if snapshot.recovery_intent => {
            let expected = snapshot
                .recovery_selected_nest_id
                .as_deref()
                .and_then(decode_nest_actor_id_hex)
                .ok_or_else(|| OnboardingError::InvalidTransition {
                    from: "vps_config".into(),
                    reason: "recovery-mode provisioning with no box selected".into(),
                })?;
            if nest_actor_id_from_seed_hex(&deployment_seed) != Some(expected) {
                machine.mutate(|s| s.is_loading = false);
                return Err(OnboardingError::ProvisioningFailed {
                    step: "recovery-config".into(),
                    detail: "the custodied deployment seed does not derive to the \
                             selected box's identity; refusing to re-provision \
                             with corrupt custody"
                        .into(),
                });
            }
            machine.hold_first_contact_identity(&domain, expected);
        }
        None => machine.hold_first_contact_root(&domain, &deployment_seed),
    }
    let secret_hex = machine.effective_secret().unwrap_or_default();
    let claim_code = match (retained.as_ref(), slot_store.as_ref()) {
        // Resuming: re-custody the slot exactly as held (the code comes back
        // out of the store's read-back, as at the mint) — never a fresh code.
        (Some(row), Some(store)) => {
            fauna_launch_machine::persist_pending_provision(store.as_ref(), secret_hex, row.clone())
                .map(|back| back.claim_code)
        }
        (Some(row), None) => Some(row.claim_code.clone()),
        (None, Some(store)) => fauna_launch_machine::mint_and_persist_pending_provision(
            store.as_ref(),
            secret_hex,
            slot_nest_url.clone(),
            slot_handle.clone(),
            fresh_identity.clone(),
            machine.pinned_claim_code(),
        ),
        (None, None) => Some(
            machine
                .pinned_claim_code()
                .unwrap_or_else(fauna_provisioning::generate_claim_code),
        ),
    };
    let Some(claim_code) = claim_code else {
        // The store swallowed the write. Building a box now would pin a claim
        // code into cloud-init that survives nowhere — the exact orphan this
        // slot exists to prevent — so refuse before spending the user's money,
        // and say which step failed.
        machine.mutate(|s| s.is_loading = false);
        return Err(OnboardingError::ProvisioningFailed {
            step: "pending_provision_slot".into(),
            detail: "the claim code could not be persisted; refusing to \
                     build a box whose claim code would not survive a crash"
                .into(),
        });
    };
    if retained.is_none() {
        machine.retain_pending_provision(AwaitingDnsRecord {
            nest_url: slot_nest_url.clone(),
            handle: slot_handle.clone(),
            dns_records_json: String::new(),
            claim_code: claim_code.clone(),
            reach_ipv4: None,
            nest_actor_id: fresh_identity.clone(),
        });
    }

    let server_name = domain.replace('.', "-");
    // DKIM is NOT generated client-side. The nest mints and holds its own
    // DKIM signing key when the mail domain is added, and the DKIM TXT is
    // published post-boot from the nest's selector list via the admin-dns
    // page (`docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM
    // provisioning). Embedding a client-generated key in cloud-init would
    // publish one the nest never signs with — `dkim=fail` from day one.
    // Mail-vs-social intent for the box: the user's explicit
    // `vps-config-mail-mode-toggle` choice if set, else the handle's real-domain
    // default (mail ON for a real registerable domain — the same predicate that
    // seeds the §3b enable-email default). A social-only box (`false`) provisions
    // a lean nest+watchtower compose with no scanner sidecars, viable on the 1 GB
    // VPS tier; it cannot later enable mail without a resize (a documented
    // constraint). `docs/goal/behavior/onboarding.md` §5.
    let enable_mail = snapshot.vps.enable_mail.unwrap_or_else(|| {
        fauna_provisioning::probe::resolve_handle_domain(&domain).is_public_dns_name
    });
    // `deployment_seed` (resolved above by `resolve_deployment_seed_and_domain`):
    // - Normal path: a **fresh** seed the admin's client mints as the box's
    //   identity **origin**, injected as `FAUNA_DEPLOYMENT_SEED` so the box boots
    //   with this `nest_actor_id`. Custodied off-box automatically once the admin
    //   is connected: the deployment-seed custody leg fetches the box's own seed
    //   (`fauna.admin.deployment_seed.get`) and merges it into
    //   `fauna.state.deployment-seeds` (box-recovery.md § The plane-era recovery
    //   floor). We deliberately do NOT custody the generated value here: the box
    //   is **ground truth** for its own identity, so if env injection ever
    //   silently failed the leg custodies the *real* seed. Total-loss recovery
    //   of a never-claimed box is moot (no user data), so the pre-claim
    //   uncustodied window is benign.
    // - Recovery path: the box's **custodied** seed (resolved from the plane's
    //   custody map, `crate::recovery_config`), so
    //   the rebuilt box re-presents the same identity every TOFU-pinned client
    //   already trusts.
    // `image_tag` is bucket-1 IPC, not a human config surface: production always
    // provisions `latest` (the closed-alpha channel Watchtower tracks), and the
    // live-provision e2e pins a specific image via `set_provision_image_tag`
    // (from `FAUNA_E2E_IMAGE_TAG`). Read the slot rather than the bare literal.
    // An unset slot — every production run — means the tag is the one the
    // user's `vps-config-update-channel-row` choice maps to.
    let image_tag = machine
        .provision_image_tag
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| machine.provision_update_channel().image_tag().to_string());
    // Extra provider-side labels for the created box — empty in production, set
    // to `[("fauna-e2e","1")]` by the live-provision e2e for a `label_selector`
    // sweep. Threaded through the orchestrator to `create_server`; the
    // orchestrator unions in the constant `managed-by=fauna` marker itself.
    let provision_labels = machine
        .provision_labels
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let params = fauna_provisioning::cloud_init::CloudInitParams {
        domain: domain.clone(),
        image_tag,
        watchtower_poll: 60,
        claim_code: claim_code.clone(),
        enable_mail,
        deployment_seed: Some(deployment_seed),
    };

    // Reset the provisioning snapshot for this new run.
    *machine.provisioning.lock().unwrap() = ProvisioningSnapshot::idle();
    machine.provisioning_cancel.reset();
    machine.observer.on_changed();

    machine.mutate(|s| {
        s.is_loading = true;
    });

    let vps_pid = snapshot.vps.selected_provider_id.clone().ok_or_else(|| {
        OnboardingError::InvalidTransition {
            from: "vps_config".into(),
            reason: "no VPS provider".into(),
        }
    })?;
    let vps_id = dispatch_provider_id(&vps_pid).ok_or_else(|| OnboardingError::Other {
        detail: format!("unknown provider id: {vps_pid}"),
    })?;
    let vps = dispatch::vps_provider(
        vps_id,
        dispatch::Credentials::from_map(snapshot.vps.creds.clone()),
        vps_base.clone(),
    )
    .ok_or_else(|| OnboardingError::Other {
        detail: format!("provider {vps_pid} doesn't support VPS"),
    })?;

    // The wizard surfaces a region picker on vps_config; fall back to the
    // captured locations' first entry if the picker hasn't fired yet.
    let region = snapshot
        .vps
        .selected_location_id
        .clone()
        .or_else(|| snapshot.vps.locations.first().map(|l| l.id.clone()))
        .ok_or_else(|| OnboardingError::InvalidTransition {
            from: "vps_config".into(),
            reason: "no VPS location selected and no locations available".into(),
        })?;

    let observer_for_notify = machine.observer.clone();
    let notify = move || observer_for_notify.on_changed();
    // Completes the slot the write above landed, the moment the box exists —
    // and settles which identity it boots with (`note_server_ready`). One
    // closure; exactly one of the three paths below takes it.
    let on_server_ready = {
        let machine = machine.clone();
        let (domain, url, handle, code, fresh, prior) = (
            domain.clone(),
            slot_nest_url.clone(),
            slot_handle.clone(),
            claim_code.clone(),
            fresh_identity.clone(),
            prior_identity,
        );
        move |ipv4: &str, origin: ServerOrigin| {
            machine.note_server_ready(
                ipv4,
                origin,
                &domain,
                &url,
                &handle,
                &code,
                fresh.as_deref(),
                prior.as_deref(),
            )
        }
    };
    let result: Result<NextStep, OnboardingError> = if snapshot.dns.set_up_later {
        // Path 1: provision_nest_no_dns (deferred-DNS — steps 1, 3, 4 Skipped).
        match fauna_provisioning::orchestrator::provision_nest_no_dns(
            &http,
            &vps,
            &domain,
            &server_name,
            &region,
            &st.id,
            &params,
            &provision_labels,
            &machine.provisioning,
            notify,
            on_server_ready,
            &machine.provisioning_cancel,
        )
        .await
        {
            Ok(deferred) => {
                let records = deferred
                    .records
                    .iter()
                    .map(DnsRecordPlain::from)
                    .collect::<Vec<_>>();
                machine.mutate(|s| {
                    s.dns_records = records;
                });
                Ok(NextStep::DnsPostInstructions)
            }
            Err(e) => Err(OnboardingError::ProvisioningFailed {
                step: "create_server".into(),
                detail: e.to_string(),
            }),
        }
    } else if snapshot.dns.buy_domain {
        // Path 2: provision_with_registration. Refuse to advance if the
        // user hasn't explicitly agreed to the displayed price — this
        // prevents the registrar from being charged without consent.
        if !snapshot.dns.price_agreed {
            machine.mutate(|s| s.is_loading = false);
            return Err(OnboardingError::InvalidTransition {
                from: "vps_config".into(),
                reason: "buy-domain flow requires confirm_price() first".into(),
            });
        }
        let agreed_cents = match snapshot.dns.current_availability.as_ref() {
            Some(fauna_provisioning::registrar::RegistrarAvailability::Buyable {
                price_cents,
                ..
            }) => *price_cents,
            _ => {
                machine.mutate(|s| s.is_loading = false);
                return Err(OnboardingError::InvalidTransition {
                    from: "vps_config".into(),
                    reason: "buy-domain flow requires a buyable availability".into(),
                });
            }
        };
        let dns_pid = snapshot.dns.selected_provider_id.clone().ok_or_else(|| {
            OnboardingError::InvalidTransition {
                from: "vps_config".into(),
                reason: "no DNS provider for buy-domain flow".into(),
            }
        })?;
        let dns_id = dispatch_provider_id(&dns_pid).ok_or_else(|| OnboardingError::Other {
            detail: format!("unknown provider id: {dns_pid}"),
        })?;
        let dns = dispatch::dns_provider(
            dns_id,
            dispatch::Credentials::from_map(snapshot.dns.creds.clone()),
            dns_base.clone(),
        )
        .ok_or_else(|| OnboardingError::Other {
            detail: format!("provider {dns_pid} doesn't support DNS"),
        })?;
        let registrar = dispatch::registrar(
            dns_id,
            dispatch::Credentials::from_map(snapshot.dns.creds.clone()),
        )
        .ok_or_else(|| OnboardingError::Other {
            detail: format!("provider {dns_pid} doesn't support registrar"),
        })?;

        let observer_for_notify2 = machine.observer.clone();
        let notify2 = move || observer_for_notify2.on_changed();

        let nest_url = nest_base
            .clone()
            .unwrap_or_else(|| format!("https://{domain}"));
        let probe_host = domain.clone();
        let probe_for_ip = move |ipv4: &str| nest_probe_client_resolving(&probe_host, ipv4);
        match fauna_provisioning::orchestrator::provision_with_registration_snapshot(
            &http,
            probe_for_ip,
            &vps,
            &dns,
            &registrar,
            &domain,
            &nest_url,
            1,
            agreed_cents,
            snapshot.dns.contact.as_ref(),
            &server_name,
            &region,
            &st.id,
            &params,
            &provision_labels,
            &machine.provisioning,
            notify2,
            on_server_ready,
            &machine.provisioning_cancel,
            |ms| async move { sleep_ms(ms).await },
        )
        .await
        {
            Ok(_) => Ok(NextStep::Provisioned { url: nest_url }),
            Err(e) => Err(OnboardingError::ProvisioningFailed {
                step: "register+provision".into(),
                detail: e.to_string(),
            }),
        }
    } else {
        // Path 3: provision (DNS pre-existing, no domain registration)
        let dns_pid = snapshot.dns.selected_provider_id.clone().ok_or_else(|| {
            OnboardingError::InvalidTransition {
                from: "vps_config".into(),
                reason: "no DNS provider in standard flow".into(),
            }
        })?;
        let dns_id = dispatch_provider_id(&dns_pid).ok_or_else(|| OnboardingError::Other {
            detail: format!("unknown provider id: {dns_pid}"),
        })?;
        let dns = dispatch::dns_provider(
            dns_id,
            dispatch::Credentials::from_map(snapshot.dns.creds.clone()),
            dns_base.clone(),
        )
        .ok_or_else(|| OnboardingError::Other {
            detail: format!("provider {dns_pid} doesn't support DNS"),
        })?;

        let zone_id = snapshot.dns.zone_id.clone().unwrap_or_default();
        let observer_for_notify3 = machine.observer.clone();
        let notify3 = move || observer_for_notify3.on_changed();

        let nest_url = nest_base
            .clone()
            .unwrap_or_else(|| format!("https://{domain}"));
        let probe_host = domain.clone();
        let probe_for_ip = move |ipv4: &str| nest_probe_client_resolving(&probe_host, ipv4);
        match fauna_provisioning::orchestrator::provision_with_snapshot(
            &http,
            probe_for_ip,
            &vps,
            &dns,
            &domain,
            &nest_url,
            &zone_id,
            &server_name,
            &region,
            &st.id,
            &params,
            &provision_labels,
            &machine.provisioning,
            notify3,
            on_server_ready,
            &machine.provisioning_cancel,
            |ms| async move { sleep_ms(ms).await },
        )
        .await
        {
            Ok(_) => Ok(NextStep::Provisioned { url: nest_url }),
            Err(e) => Err(OnboardingError::ProvisioningFailed {
                step: "provision".into(),
                detail: e.to_string(),
            }),
        }
    };

    // Provisioning = build + claim (`docs/goal/behavior/onboarding.md` § 6): on
    // the standard path a built box is not yet a *provisioned* one — `Succeeded`
    // must mean built **and claimed**, so the claim runs here, as `Online`'s final
    // substep, before this returns. Without it every real user of "Set up a nest
    // from the app" was signed in to an unclaimed box and bounced to a claim page
    // asking for a code they never saw.
    //
    // The deferred-DNS path deliberately does NOT claim: its `Online` step is
    // `Skipped` (no domain to poll yet), and the "Almost ready" surface runs the
    // very same core once the user's records resolve.
    let result = match result {
        Ok(NextStep::Provisioned { url }) => {
            machine.run_provisioning_claim(&url, &claim_code).await;
            Ok(NextStep::Provisioned { url })
        }
        other => other,
    };

    machine.mutate(|s| {
        s.is_loading = false;
    });
    result
}

/// Milliseconds-shaped call-site adapter over the shared cross-target sleep
/// (`fauna_sleep::sleep`), passed as the orchestrator's `sleep_fn` closure.
///
/// ⚠ The argument is **milliseconds** — `run_step` passes the retry backoff
/// from `RetryPolicy::initial_backoff_ms` (e.g. 5000 for the Online step).
/// An earlier version named this `sleep_secs` and re-multiplied by 1000,
/// turning the 5 s Online backoff into 5000 s (~83 min) — provisioning's
/// retry/cancel path hung on the first non-200 health poll (web especially,
/// where the nest legitimately takes seconds to come online). That unit slip is
/// exactly why the shared helper takes a `Duration` and nothing else.
async fn sleep_ms(ms: u64) {
    fauna_sleep::sleep(std::time::Duration::from_millis(ms)).await;
}

// `fauna_provisioning::generate_claim_code` — pulled via use site —
// uses a CSPRNG instead of subsec_nanos.

fn dispatch_provider_id(s: &str) -> Option<fauna_provisioning::ProviderId> {
    fauna_provisioning::ProviderId::from_str(s)
}

// Module-scope helpers for the handle-check flow.

/// The nest-discovery domain of a wizard handle: `nest.example` from
/// `alice@nest.example`. `None` for anything that is not the `user@domain` form
/// — a bare `alice`, an `@domain` form, an empty side.
///
/// Delegates to the canonical [`fauna_core::resolve::parse_handle`] rather than
/// re-splitting on `@` (priority #2/#4: one owner for handle parsing). Same
/// results as the hand-rolled split it replaced on every input, and it inherits
/// that parser's rejection of a second `@` — which matters here, because this
/// domain is dialled: `a@b@c` used to yield `b@c`, a string a URL parser reads
/// as userinfo `b@` plus host `c`, so the wizard displayed one nest and
/// connected to another (`security.md` § Transport trust).
fn parse_handle_domain(handle: &str) -> Option<String> {
    match fauna_core::resolve::parse_handle(handle) {
        fauna_core::resolve::HandleForm::Named { domain, .. } => Some(domain),
        _ => None,
    }
}

/// The registerable local part of a wizard handle: `alice` from `alice` or
/// `alice@nest.example`. The nest stores the bare local part (its handle
/// validation rejects `@`); the `@domain` suffix a handle may carry is only for
/// nest discovery, not part of the stored handle. Returns the whole string when
/// there is no `@`.
fn handle_local_part(handle: &str) -> &str {
    handle.split('@').next().unwrap_or(handle)
}

// Ed25519 helpers used by the wizard's invite-request / register /
// challenge wire calls. The secret is the 64-hex string the user typed
// on `identity_import` or generated on `identity_created`; it's also
// what `confirm_*_identity` returns and what per-app glue persists.
//
// These are real Ed25519 (derive + sign) against Spec 2's nest-side
// challenge wire (tracked internally; see also
// bins/fauna-nest/src/invite_requests.rs + bins/fauna-nest/src/registration.rs).

/// Parse a 64-char hex secret into an Ed25519 SigningKey.
/// Returns `None` for invalid input (wrong length, non-hex chars).
fn parse_signing_key(secret_hex: &str) -> Option<ed25519_dalek::SigningKey> {
    let array = fauna_core::hex32::decode(secret_hex).ok()?;
    Some(ed25519_dalek::SigningKey::from_bytes(&array))
}

/// Returns the Ed25519 public key (actor_id) as a 64-char lowercase hex string.
/// `""` on parse failure — callers should pre-validate the secret with
/// `confirm_imported_identity` before reaching this path.
fn derive_pubkey(secret_hex: &str) -> String {
    match parse_signing_key(secret_hex) {
        Some(sk) => hex::encode(sk.verifying_key().to_bytes()),
        None => String::new(),
    }
}

/// Ed25519-sign an arbitrary message with the given secret.
/// Returns the 128-char hex signature, or `""` on parse failure.
fn sign_bytes(secret_hex: &str, message: &[u8]) -> String {
    use ed25519_dalek::Signer;
    match parse_signing_key(secret_hex) {
        Some(sk) => hex::encode(sk.sign(message).to_bytes()),
        None => String::new(),
    }
}

/// Build the signed `fauna.setup.nat_mode` wire body for `mode`, bound to
/// the nest `bound_nest_id_hex` names. The signature covers the canonical
/// bytes `mode_wire_str || "\n" || actor_id_hex || "\n" ||
/// timestamp_decimal || "\n" || nest_id_hex`
/// (`fauna_protocol::nat_mode::nat_mode_signed_message`; the nest-bound
/// form, `transport-connection.md`) — verified by the nest's
/// `commit_nat_mode_core`. Shared by the wizard's `submit_nat_mode_choice`
/// and the admin panel's [`crate::AdminNatModeMachine`] via
/// `WsRpcNestApi::submit_nat_mode`, which reads the identity off the
/// connection, so the two app surfaces can never drift on the commit
/// ceremony. `None` for an unparseable secret.
pub(crate) fn build_signed_nat_mode_body(
    secret_hex: &str,
    mode: NodeMode,
    bound_nest_id_hex: &str,
) -> Option<crate::nest_api::NatModeBody> {
    let signing_key = parse_signing_key(secret_hex)?;
    let actor_id_hex = hex::encode(signing_key.verifying_key().to_bytes());
    let timestamp_ms = Timestamp::now_millis() as i64;
    let signed_bytes = fauna_protocol::nat_mode::nat_mode_signed_message(
        mode.as_str(),
        &actor_id_hex,
        timestamp_ms,
        bound_nest_id_hex,
    );
    let signature_hex = sign_bytes(secret_hex, &signed_bytes);
    Some(crate::nest_api::NatModeBody {
        mode,
        actor_id: actor_id_hex,
        timestamp: timestamp_ms,
        signature: signature_hex,
        nest_id: bound_nest_id_hex.to_owned(),
    })
}

/// Build the signed `fauna.account.invite_request.cancel` body — the withdrawal
/// a denied requester's resubmit runs first (`onboarding.md` § The
/// pending-invite surface).
///
/// The signature covers the domain-tagged
/// `fauna_protocol::invite::invite_cancel_signed_message` — the single-source
/// builder the nest's `cancel_invite_request_core` verifies with. `None` on a
/// secret hex that isn't a usable Ed25519 key.
pub(crate) fn build_invite_request_cancel(
    secret_hex: &str,
) -> Option<fauna_protocol::invite::InviteRequestCancel> {
    let sk = parse_signing_key(secret_hex)?;
    let actor_id_bytes = sk.verifying_key().to_bytes();
    let timestamp = Timestamp::now_millis();

    let msg = fauna_protocol::invite::invite_cancel_signed_message(&actor_id_bytes, timestamp);

    Some(fauna_protocol::invite::InviteRequestCancel {
        actor_id: hex::encode(actor_id_bytes),
        timestamp,
        signature: sign_bytes(secret_hex, &msg),
        extra: Default::default(),
    })
}

/// `invite-request-continue-button`'s enabled condition, in one place because
/// four sites recompute it (`onboarding.md` § 3 — the button's row).
///
/// Must track exactly the condition [`OnboardingMachine::redeem_invite`] routes
/// on, so a control is never live with nothing behind it.
///
/// As of 2026-08-12 that is the out-of-band code **alone** (`onboarding.md` § 3
/// — the button's row). The two former disjuncts both retired with the
/// continue-exit: `PendingReview` because that journey advances by polling, not
/// by a button (§ The pending-invite surface), and `Approved` because no live
/// nest ever serves it. Leaving either in would light a control with nothing
/// behind it — the exact failure this helper exists to prevent.
fn continue_enabled_for(oob: &crate::snapshots::OobCodeState) -> bool {
    matches!(oob, crate::snapshots::OobCodeState::Valid { .. })
}

// The wire shape `bins/fauna-nest/src/invite_requests.rs::status_json`
// emits on `POST /api/v1/invite-requests`, the matching `GET /status`,
// and the admin approve/deny endpoints is now
// `crate::nest_api::InviteRequestResponse` — pulled into the trait
// module in Phase B0 so the wizard's snapshot builder consumes the
// trait's response type directly (no conversion layer).
fn state_from_resp(
    b: crate::nest_api::InviteRequestResponse,
) -> crate::snapshots::InviteRequestState {
    use crate::snapshots::InviteRequestState;
    let request_id = b.id.to_string();
    match b.status.as_str() {
        // ⚠ There is deliberately no `"approved"` arm (retired 2026-08-12 with
        // `InviteRequestState::Approved`). The admin approve deletes the row,
        // so approval arrives as the row's ABSENCE and is resolved by the
        // registered-probe, never by a status string (`onboarding.md` § The
        // pending-invite surface). A nest that somehow sent `"approved"` now
        // falls through to `PendingReview` below and the next poll's probe
        // settles it — which is the correct answer, not a lost signal.
        "denied" => InviteRequestState::Denied {
            reason: b.denial_reason.unwrap_or_default(),
            request_id,
        },
        _ => InviteRequestState::PendingReview {
            request_id,
            last_checked_ms: Timestamp::now_millis(),
        },
    }
}

// ── hosted-auth: a bundled provider's hosted sign-in ───────────────────────
//
// The `hosted-auth` field type (`registry.md` § Bundled provider) is a button,
// not an input: the wizard runs the provider's RFC 8628 device-authorization
// flow (`bundled-provider-api.md` § Authentication) and the resulting Bearer
// token becomes the field's credential value. Two async calls, both here, so
// the per-app glue is exactly the button, one open-URL call, and a label
// lookup (`onboarding.md` § 4) — the same on all 7 apps, terminal included.
// The flow itself is `crate::hosted_auth`'s, shared with the retire view.

use crate::hosted_auth::PendingDeviceAuth;

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl OnboardingMachine {
    /// Step 1 of a `hosted-auth` field's sign-in: POST the device-authorization
    /// request to the form's `base-url` and hand back what the app must open
    /// (through its existing open-URL affordance) and show (the user code).
    /// The field flips to `Pending`; follow with [`Self::hosted_auth_wait`].
    pub async fn hosted_auth_begin(
        &self,
        form: CredentialForm,
        field_id: String,
    ) -> Result<HostedAuthPrompt, OnboardingError> {
        match crate::hosted_auth::begin(&self.http, self.hosted_auth_base_url(form)).await {
            Ok((prompt, pending)) => {
                self.hosted_auth_pending
                    .lock()
                    .unwrap()
                    .insert((form, field_id.clone()), pending);
                self.set_hosted_auth_state(
                    form,
                    &field_id,
                    HostedAuthState::Pending {
                        user_code: prompt.user_code.clone(),
                        verification_url: prompt.verification_url.clone(),
                    },
                );
                Ok(prompt)
            }
            Err(e) => {
                let message = e.to_string();
                self.set_hosted_auth_state(
                    form,
                    &field_id,
                    HostedAuthState::Failed {
                        message: message.clone(),
                    },
                );
                Err(OnboardingError::Other { detail: message })
            }
        }
    }

    /// Step 2: poll the token endpoint at the server's interval until the user
    /// approves (the token lands in the form's credential bag under
    /// `field_id`, the field flips to `Connected`, and `can_verify_*` turns
    /// true) or the attempt ends (`Failed`). Resolves only then — the app
    /// awaits it the way it awaits `verify_dns`.
    pub async fn hosted_auth_wait(
        &self,
        form: CredentialForm,
        field_id: String,
    ) -> Result<(), OnboardingError> {
        let pending = self
            .hosted_auth_pending
            .lock()
            .unwrap()
            .remove(&(form, field_id.clone()));
        let Some(pending) = pending else {
            return Err(OnboardingError::Other {
                detail: "no sign-in in progress for this field".into(),
            });
        };
        let label = format!("{form:?}/{field_id}");
        match crate::hosted_auth::wait(&self.http, pending, &label).await {
            Ok(token) => {
                let id = field_id.clone();
                self.mutate(|s| {
                    let (creds, states, verified) = match form {
                        CredentialForm::Dns => (
                            &mut s.dns.creds,
                            &mut s.dns.hosted_auth,
                            &mut s.dns.verified,
                        ),
                        CredentialForm::Vps => (
                            &mut s.vps.creds,
                            &mut s.vps.hosted_auth,
                            &mut s.vps.verified,
                        ),
                    };
                    // Same discipline as `set_dns_cred`: wrap at the input
                    // boundary, and a new credential un-verifies the form.
                    creds.insert(id.clone(), SecretString::from(token));
                    states.insert(id, HostedAuthState::Connected);
                    *verified = false;
                });
                Ok(())
            }
            Err(message) => {
                self.set_hosted_auth_state(
                    form,
                    &field_id,
                    HostedAuthState::Failed {
                        message: message.clone(),
                    },
                );
                Err(OnboardingError::Other { detail: message })
            }
        }
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl OnboardingMachine {
    /// Where a `hosted-auth` field's sign-in stands — the app's button label
    /// source (`onboarding.md` § 4). `Idle` for a field never started.
    pub fn hosted_auth_state(&self, form: CredentialForm, field_id: String) -> HostedAuthState {
        self.with_state(|s| {
            let states = match form {
                CredentialForm::Dns => &s.dns.hosted_auth,
                CredentialForm::Vps => &s.vps.hosted_auth,
            };
            states
                .get(&field_id)
                .cloned()
                .unwrap_or(HostedAuthState::Idle)
        })
    }

    /// Whether the sign-in button is pressable: the form's `base-url` is
    /// filled in and no attempt on this field is mid-flight. Owned here so no
    /// app re-derives "which sibling field is the address" (priority #2).
    pub fn hosted_auth_can_begin(&self, form: CredentialForm, field_id: String) -> bool {
        self.hosted_auth_base_url(form).is_some()
            && !matches!(
                self.hosted_auth_state(form, field_id),
                HostedAuthState::Pending { .. }
            )
    }

    /// The `hosted-auth` button's label for one field, derived purely from
    /// [`Self::hosted_auth_state`] — the shared-Rust twin of the match arm
    /// tui's `hosted_auth_button` and linux's `paint_hosted_auth_button` each
    /// hand-copied and self-documented as a "mirror" of the other
    /// (`onboarding.md` § 4). android/apple/web keep their own copy: each
    /// binds a different platform i18n surface (`R.string.*`, a Swift
    /// `LocalizedStringKey`, a reactive TS `t.*`), so the enum→label mapping
    /// still has to be re-expressed per platform — only the two Rust-native
    /// apps could actually call one function.
    pub fn hosted_auth_button_text(&self, form: CredentialForm, field_id: String) -> String {
        crate::hosted_auth::button_text(&self.hosted_auth_state(form, field_id))
    }
}

impl OnboardingMachine {
    fn hosted_auth_base_url(&self, form: CredentialForm) -> Option<String> {
        self.with_state(|s| {
            let creds = match form {
                CredentialForm::Dns => &s.dns.creds,
                CredentialForm::Vps => &s.vps.creds,
            };
            creds
                .get(crate::hosted_auth::BASE_URL_FIELD)
                .map(|v| v.as_str().trim().to_string())
                .filter(|v| !v.is_empty())
        })
    }

    fn set_hosted_auth_state(&self, form: CredentialForm, field_id: &str, state: HostedAuthState) {
        let id = field_id.to_string();
        self.mutate(|s| {
            let states = match form {
                CredentialForm::Dns => &mut s.dns.hosted_auth,
                CredentialForm::Vps => &mut s.vps.hosted_auth,
            };
            states.insert(id, state);
        });
    }
}

#[cfg(test)]
mod hosted_auth_tests {
    //! The device-authorization flow end to end against a wiremock
    //! intermediary — what every app's hosted-auth button relies on.
    use std::sync::Arc;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::observer::CountingObserver;

    fn device_json(interval: u64) -> serde_json::Value {
        serde_json::json!({
            "device_code": "dev-1",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://bundle.example/activate",
            "verification_uri_complete": "https://bundle.example/activate?user_code=ABCD-EFGH",
            "expires_in": 60,
            "interval": interval
        })
    }

    #[tokio::test]
    async fn begin_then_wait_lands_the_token_in_the_forms_credential_bag() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(device_json(0)))
            .mount(&server)
            .await;
        // First poll: pending; second: the token.
        Mock::given(method("POST"))
            .and(path("/v1/auth/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "authorization_pending" })),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "tok-final", "token_type": "bearer" }),
            ))
            .mount(&server)
            .await;

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.select_dns_provider("bundled".into());
        assert!(
            !m.hosted_auth_can_begin(CredentialForm::Dns, "api-token".into()),
            "no base-url yet → the button is dead"
        );
        m.set_dns_cred("base-url".into(), format!("{}/", server.uri()));
        assert!(m.hosted_auth_can_begin(CredentialForm::Dns, "api-token".into()));

        let prompt = m
            .hosted_auth_begin(CredentialForm::Dns, "api-token".into())
            .await
            .unwrap();
        assert_eq!(prompt.user_code, "ABCD-EFGH");
        assert_eq!(
            prompt.verification_url,
            "https://bundle.example/activate?user_code=ABCD-EFGH"
        );
        assert!(matches!(
            m.hosted_auth_state(CredentialForm::Dns, "api-token".into()),
            HostedAuthState::Pending { ref user_code, .. } if user_code == "ABCD-EFGH"
        ));
        assert!(
            !m.hosted_auth_can_begin(CredentialForm::Dns, "api-token".into()),
            "mid-flight → not re-pressable"
        );

        m.hosted_auth_wait(CredentialForm::Dns, "api-token".into())
            .await
            .unwrap();
        assert_eq!(
            m.hosted_auth_state(CredentialForm::Dns, "api-token".into()),
            HostedAuthState::Connected
        );
        assert_eq!(
            m.dns_config()
                .creds
                .get("api-token")
                .map(|v| v.as_str().to_string()),
            Some("tok-final".into())
        );
        assert!(
            m.can_verify_dns(),
            "base-url + token filled → the verify button lights up"
        );
        // And the VPS form inherits both the token and the Connected state.
        m.toggle_same_provider_for_vps(true);
        m.continue_from_dns().unwrap();
        assert_eq!(
            m.hosted_auth_state(CredentialForm::Vps, "api-token".into()),
            HostedAuthState::Connected
        );
    }

    /// Spec § Endpoints requires `https://`. A non-loopback
    /// `http://` base-url must fail here, as a field error the user can read
    /// and correct — never reach `device_authorize`'s network call (no mock
    /// is mounted for `provider.example`, so a dropped guard would hang or
    /// DNS-fail instead of returning this typed field error).
    #[tokio::test]
    async fn hosted_auth_begin_refuses_a_non_https_base_url_as_a_field_error() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.select_dns_provider("bundled".into());
        m.set_dns_cred("base-url".into(), "http://provider.example".into());
        let err = m
            .hosted_auth_begin(CredentialForm::Dns, "api-token".into())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("https://"), "{err}");
        assert!(matches!(
            m.hosted_auth_state(CredentialForm::Dns, "api-token".into()),
            HostedAuthState::Failed { ref message } if message.contains("https://")
        ));
        assert!(
            !m.can_verify_dns(),
            "a refused sign-in must not leave the form ready to verify"
        );
    }

    #[tokio::test]
    async fn a_declined_sign_in_fails_the_field_and_leaves_no_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(device_json(0)))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "access_denied" })),
            )
            .mount(&server)
            .await;
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.select_dns_provider("bundled".into());
        m.set_dns_cred("base-url".into(), server.uri());
        m.hosted_auth_begin(CredentialForm::Dns, "api-token".into())
            .await
            .unwrap();
        let err = m
            .hosted_auth_wait(CredentialForm::Dns, "api-token".into())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("declined"), "{err}");
        assert!(matches!(
            m.hosted_auth_state(CredentialForm::Dns, "api-token".into()),
            HostedAuthState::Failed { .. }
        ));
        assert!(!m.dns_config().creds.contains_key("api-token"));
        assert!(
            m.hosted_auth_can_begin(CredentialForm::Dns, "api-token".into()),
            "a failed attempt is re-pressable"
        );
    }

    #[tokio::test]
    async fn the_poll_budget_expires_a_never_approved_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-1", "user_code": "X", "verification_uri": "https://b/x",
                "expires_in": 2, "interval": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/auth/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "authorization_pending" })),
            )
            .mount(&server)
            .await;
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.select_dns_provider("bundled".into());
        m.set_dns_cred("base-url".into(), server.uri());
        m.hosted_auth_begin(CredentialForm::Dns, "api-token".into())
            .await
            .unwrap();
        let err = m
            .hosted_auth_wait(CredentialForm::Dns, "api-token".into())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expired"), "{err}");
        // expires_in / interval = 2 polls, then give up — never a third.
        assert_eq!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|r| r.url.path() == "/v1/auth/token")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn selecting_another_provider_forgets_the_sign_in() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.select_dns_provider("bundled".into());
        m.set_dns_state_for_test(|d| {
            d.hosted_auth
                .insert("api-token".into(), HostedAuthState::Connected);
        });
        m.select_dns_provider("porkbun".into());
        assert_eq!(
            m.hosted_auth_state(CredentialForm::Dns, "api-token".into()),
            HostedAuthState::Idle
        );
    }

    /// The button-text derivation tui's `hosted_auth_button` and linux's
    /// `paint_hosted_auth_button` now both call instead of hand-matching —
    /// pins all four `HostedAuthState` arms against the generated i18n
    /// constants directly, so a future arm added to one without the other
    /// fails to compile before it ever reaches an app.
    #[tokio::test]
    async fn hosted_auth_button_text_covers_every_state() {
        use fauna_i18n::strings::provisioning::hosted_auth as h;

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.select_dns_provider("bundled".into());
        assert_eq!(
            m.hosted_auth_button_text(CredentialForm::Dns, "api-token".into()),
            h::CONNECT
        );

        m.set_dns_state_for_test(|d| {
            d.hosted_auth.insert(
                "api-token".into(),
                HostedAuthState::Pending {
                    user_code: "ABCD-EFGH".into(),
                    verification_url: "https://bundle.example/activate".into(),
                },
            );
        });
        assert_eq!(
            m.hosted_auth_button_text(CredentialForm::Dns, "api-token".into()),
            h::pending("ABCD-EFGH")
        );

        m.set_dns_state_for_test(|d| {
            d.hosted_auth
                .insert("api-token".into(), HostedAuthState::Connected);
        });
        assert_eq!(
            m.hosted_auth_button_text(CredentialForm::Dns, "api-token".into()),
            h::CONNECTED
        );

        m.set_dns_state_for_test(|d| {
            d.hosted_auth.insert(
                "api-token".into(),
                HostedAuthState::Failed {
                    message: "access_denied".into(),
                },
            );
        });
        assert_eq!(
            m.hosted_auth_button_text(CredentialForm::Dns, "api-token".into()),
            h::failed("access_denied")
        );
    }
}

#[cfg(test)]
mod free_method_tests {
    //! The machine-free E2E-bridge dispatcher (`call_machine_free_method`) — the
    //! seam the post-auth "nest identity changed" e2e needs, because a live
    //! authenticated session has no `OnboardingMachine` to route the pin seed
    //! through. The pin store is process-global, so each
    //! test uses a distinct nest URL to stay independent under the parallel
    //! test runner.
    use super::{FreeMethodOutcome, call_machine_free_method};

    const A_PIN: &str = "ab";
    fn bogus_pin_hex() -> String {
        A_PIN.repeat(32)
    }

    #[test]
    fn seed_then_read_round_trips_through_the_free_dispatcher_without_a_machine() {
        let nest = "https://free-dispatcher-roundtrip.example:8443";
        let seed = serde_json::json!({ "nest_url": nest, "actor_id": bogus_pin_hex() });

        // Seed: a setter, so `Handled(None)`.
        match call_machine_free_method("set_nest_identity_pin_for_test", &seed.to_string()) {
            FreeMethodOutcome::Handled(v) => assert_eq!(v, None, "the seed is a setter"),
            FreeMethodOutcome::NeedsMachine => {
                panic!("the pin seed must be machine-free — that is the whole point of the seam")
            }
        }

        // Read: the reader returns the JSON-serialized `Option<hex>`.
        let query = serde_json::json!({ "nest_url": nest });
        match call_machine_free_method("nest_identity_pin_for_test", &query.to_string()) {
            FreeMethodOutcome::Handled(Some(json)) => {
                let got: Option<String> = serde_json::from_str(&json).expect("reader emits JSON");
                assert_eq!(
                    got,
                    Some(bogus_pin_hex()),
                    "the reader must see the pin the seed just wrote — same process-global store"
                );
            }
            FreeMethodOutcome::Handled(None) => panic!("the reader must return a value, got None"),
            FreeMethodOutcome::NeedsMachine => {
                panic!("the pin reader must be machine-free")
            }
        }
    }

    #[test]
    fn reader_returns_json_null_when_no_pin_is_held() {
        let query = serde_json::json!({ "nest_url": "https://never-seeded.example:9000" });
        match call_machine_free_method("nest_identity_pin_for_test", &query.to_string()) {
            FreeMethodOutcome::Handled(Some(json)) => {
                let got: Option<String> = serde_json::from_str(&json).expect("reader emits JSON");
                assert_eq!(got, None, "no pin held → JSON null");
            }
            _ => panic!("reader must be machine-free and return a value"),
        }
    }

    #[test]
    fn a_machine_requiring_or_unknown_name_reports_needs_machine() {
        // A real machine-requiring bridge method and a nonsense name both fall
        // through — the caller must route them to a live machine (or, with none,
        // fail loudly rather than silently drop, per convention 11).
        for name in ["set_step_for_test", "seed_identity", "totally_unknown"] {
            assert!(
                matches!(
                    call_machine_free_method(name, "\"x\""),
                    FreeMethodOutcome::NeedsMachine
                ),
                "'{name}' is not machine-free and must report NeedsMachine"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::CountingObserver;

    /// Minimal capturing `tracing::Layer` — mirrors
    /// `fauna_provisioning::progress`'s own test-only copy; small enough (and
    /// crate-local enough) that duplicating it beats a cross-crate test-only
    /// dependency. Runs `f` under a subscriber that filters at `info` (matching
    /// `fauna-log`'s real default, `EnvFilter::new("info")`), so a `debug!` line
    /// is exercised exactly the way it is invisible in production.
    ///
    /// exposure check: this helper's
    /// only two callers below are the sole callers of `nest_probe_client_resolving`
    /// in this test binary — the two other tests that drive `run_provisioning_inner`
    /// (`recovery`/custody-mismatch cases) set `dns.buy_domain = false` or error
    /// out before the buy-domain branch that reaches the probe closure, so no
    /// sibling test can hit `nest_probe_client_resolving`'s `tracing::warn!`/
    /// `info!` callsites with no subscriber installed. Demonstrated unexposed;
    /// the thread-local `with_default` here is not a defect.
    fn capture_tracing_at_info<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
        use tracing_subscriber::prelude::*;

        struct CaptureLayer(Arc<Mutex<Vec<String>>>);

        #[derive(Default)]
        struct MessageOnly(String);

        impl tracing::field::Visit for MessageOnly {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }

        impl<S> tracing_subscriber::Layer<S> for CaptureLayer
        where
            S: tracing::Subscriber,
        {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                let mut visitor = MessageOnly::default();
                event.record(&mut visitor);
                let line = format!(
                    "[{}][{}] {}",
                    event.metadata().level(),
                    event.metadata().target(),
                    visitor.0
                );
                self.0.lock().unwrap().push(line);
            }
        }

        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("info"))
            .with(CaptureLayer(Arc::clone(&captured)));
        let out = tracing::subscriber::with_default(subscriber, f);
        let lines = captured.lock().unwrap().clone();
        (out, lines)
    }

    /// Regression guard for the question a live windows provisioning run could not answer: was the poll reaching the
    /// box via its captured-IP DNS override, or had it silently fallen back to
    /// public DNS? The unusable-address arm already warns (asserted here as the
    /// capturing-subscriber baseline); the usable-address arm used to log the
    /// choice at `debug!` — invisible under the default `info` filter — which is
    /// exactly why that question was unanswerable from the captured log.
    #[test]
    fn probe_reach_choice_is_visible_at_the_default_info_filter() {
        let (_client, lines) =
            capture_tracing_at_info(|| nest_probe_client_resolving("host.example", "not-an-ip"));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[WARN]") && l.contains("not dialable")),
            "expected the not-dialable fallback warning, got:\n{}",
            lines.join("\n")
        );

        let (_client, lines) =
            capture_tracing_at_info(|| nest_probe_client_resolving("host.example", "203.0.113.7"));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[INFO]") && l.contains("203.0.113.7:443")),
            "expected an info-level line naming the reach socket 203.0.113.7:443, got:\n{}",
            lines.join("\n")
        );
    }

    /// The reach-override decision is named, so a live run can say which client
    /// the Online poll actually built.
    ///
    /// `nest_probe_client_resolving` falls back to the override-less client when
    /// the captured address does not parse — correct, but until 2026-08-31 it was
    /// silent, so a poll dialing public DNS instead of the box's captured IP was
    /// indistinguishable from one whose override was in place. That ambiguity is
    /// what a live windows provisioning run had
    /// to establish from a paid box.
    #[test]
    fn the_probe_reach_socket_is_the_captured_ip_on_443_or_an_honest_none() {
        assert_eq!(
            probe_reach_socket("203.0.113.7"),
            Some("203.0.113.7:443".parse().unwrap()),
            "a captured IPv4 must become the :443 socket the override dials",
        );
        assert_eq!(
            probe_reach_socket("2001:db8::1"),
            Some("[2001:db8::1]:443".parse().unwrap()),
            "an IPv6 box is reachable on the same port",
        );
        for unusable in ["", "  ", "not-an-ip", "203.0.113.7:443"] {
            assert_eq!(
                probe_reach_socket(unusable),
                None,
                "{unusable:?} is not a dialable address — the caller must fall back \
                 to the override-less client, and say so",
            );
        }
    }

    /// A blackholed liveness dial must give up on its own, not hang forever.
    ///
    /// The provisioning Online poll (`fauna_provisioning::orchestrator`, step 4)
    /// budgets itself as `480 × 5s` — an attempt *count* whose 40-minute framing
    /// in `ProvisionStep::default_retry_policy` only holds if a single attempt is
    /// bounded. It was not: neither probe builder set a timeout, so one dial into
    /// a silent socket consumed the whole step. Measured live on windows
    /// (2026-08-31, a real Hetzner box): `attempt 2/480` after 1200s, i.e. two
    /// attempts where the policy promises hundreds.
    ///
    /// Convention 14 (`e2e-conventions.md`): the ceiling below is sized far above
    /// the client's own budget and far below "forever", so the assertion is about
    /// *termination*, not about how fast the dial gives up.
    #[tokio::test]
    async fn a_stalled_probe_dial_is_bounded_by_the_clients_own_timeout() {
        // Completes the TCP connect, then never speaks a byte — the shape of a
        // box whose port answers before the nest serves, or of a middlebox
        // swallowing the TLS handshake. The client's ClientHello gets no reply.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept() {
                held.push(stream);
            }
        });

        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            nest_probe_client()
                .get(format!("https://127.0.0.1:{port}/api/v1/health"))
                .send(),
        )
        .await;

        assert!(
            outcome.is_ok(),
            "the probe dial never returned within 60s: the client carries no timeout \
             ceiling, so ONE Online-poll attempt can outlast the entire step budget \
             (elapsed {:?})",
            started.elapsed(),
        );
        assert!(
            outcome.unwrap().is_err(),
            "a silent socket must surface as an error the retry loop can act on",
        );
    }

    /// Pin: `run_provisioning_claim` MUST dial
    /// the URL it was called with (`NextStep::Provisioned { url }`, the
    /// run's own just-computed provisioning-path address), never
    /// `effective_nest_url()` / `state.nest_url` — that field is populated
    /// only later, by `stash_provisioning_result` after this function
    /// returns, so reading it here dialed a stale/empty URL and every real
    /// wizard-provisioned box's automatic claim failed silently until
    /// 2026-08-29 (see this fn's own doc comment).
    ///
    /// Seeds `state.nest_url` to a WRONG address distinct from the one
    /// passed as the parameter — not an empty string — so the probe is
    /// observably different under the two shapes regardless of what
    /// `effective_nest_url()`'s override half resolves to; no
    /// `provider_base_urls` override is installed at all
    /// (`OnboardingMachine::with_nest_api`), so `effective_nest_url()` can
    /// only ever fall through to the wrong seeded value. Mutation-graded:
    /// change `claim_provisioned_box(nest_url, ...)` at the call site to
    /// `claim_provisioned_box(&self.effective_nest_url(), ...)` and this
    /// reddens on the wrong URL — the literal shape of the bug that shipped.
    #[tokio::test]
    async fn run_provisioning_claim_dials_its_own_parameter_not_the_stale_state_field() {
        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        let machine = OnboardingMachine::with_nest_api(
            CountingObserver::new(),
            fake.clone() as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );

        // A wrong, stale URL in `state.nest_url` — exactly the shape
        // `effective_nest_url()` would read if the guard regressed. Distinct
        // from both the correct URL below and from "".
        machine.set_nest_url("https://stale-cached-url.example".into());

        let correct_url = "https://the-box-this-run-just-provisioned.example";
        machine
            .run_provisioning_claim(correct_url, "CLAIMCODE123")
            .await;

        let dialed: Vec<_> = fake
            .base_url_calls()
            .into_iter()
            .filter(|(method, _)| method == "probe_setup_status")
            .map(|(_, url)| url)
            .collect();
        assert_eq!(
            dialed,
            vec![correct_url.to_string()],
            "run_provisioning_claim must probe the URL it was called with, not the \
             stale `state.nest_url` a regression to effective_nest_url() would read"
        );
    }

    /// Regression guard for the provisioning retry-backoff unit bug. The
    /// orchestrator passes `RetryPolicy::initial_backoff_ms` (e.g. 5000)
    /// to the `sleep_fn`; an earlier `sleep_secs` re-multiplied by 1000,
    /// turning the 5 s Online backoff into 5000 s and hanging the
    /// retry/cancel path. `sleep_ms(20)` must take ~20 ms, not 20 s.
    #[tokio::test]
    async fn sleep_ms_treats_arg_as_milliseconds() {
        let start = std::time::Instant::now();
        sleep_ms(20).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "sleep_ms(20) took {elapsed:?} — should be ~20ms (ms unit), not seconds"
        );
    }

    // ── Box-recovery leg 3: recovery-mode seed/domain resolution ────────────
    //
    // `resolve_deployment_seed_and_domain` is the one branch point between
    // first-provision and recovery-mode re-provision (box-recovery.md § Recovery
    // UI (step 4), leg 3). A fake `RecoveryConfigReader` stands in for the authed
    // custody read so we can assert the recovery branch installs the
    // *custodied* seed + the box's own domain, and the normal branch still mints
    // a fresh seed + uses the handle domain.
    //
    // ⚠ These four tests are necessary but NOT sufficient — they stop at this
    // function's return value. The links onward (→ `CloudInitParams` →
    // `build_cloud_init` → `create_server`) are covered by
    // `recovery_drive_sends_the_custodied_seed_to_the_vps_create_call` below,
    // which drives the whole thing against a faked provider.

    #[derive(Debug)]
    struct FakeRecoveryReader {
        seed: [u8; 32],
        domain: Option<String>,
    }

    #[async_trait::async_trait]
    impl crate::recovery_config::RecoveryConfigReader for FakeRecoveryReader {
        async fn resolve(
            &self,
            _nest_url: Option<&str>,
            _owner_secret_hex: &str,
            _nest_actor_id_hex: &str,
        ) -> Result<
            crate::recovery_config::RecoveryProvisionInput,
            crate::recovery_config::RecoveryConfigError,
        > {
            Ok(crate::recovery_config::RecoveryProvisionInput {
                deployment_seed: self.seed,
                domain: self.domain.clone(),
            })
        }
    }

    /// Recovery mode installs the **custodied** seed of the selected box + the
    /// box's **own** domain (from custody), not a freshly-minted seed or the
    /// admin's handle domain — so the rebuilt box re-presents the same
    /// `nest_actor_id` and the A/AAAA re-point targets the box's domain.
    #[tokio::test]
    async fn resolve_recovery_mode_uses_custodied_seed_and_box_domain() {
        let m = OnboardingMachine::new(CountingObserver::new());
        let box_id_hex = "ab".repeat(32);
        let custodied_seed = [0x11u8; 32];
        {
            let mut s = m.state.lock().unwrap();
            s.recovery_intent = true;
            s.recovery_selected_nest_id = Some(box_id_hex.clone());
            s.imported_secret = Some("cd".repeat(32).into());
            s.nest_url = "https://surviving.example".into();
            // A handle domain is present but must be IGNORED in recovery mode.
            s.handle = "admin@my-handle.example".into();
        }
        *m.recovery_config_reader.write().unwrap() = Arc::new(FakeRecoveryReader {
            seed: custodied_seed,
            domain: Some("recovered-box.example".into()),
        });

        let snap = m.snapshot();
        let (domain, seed_hex) = m
            .resolve_deployment_seed_and_domain(&snap)
            .await
            .expect("recovery resolution should succeed with a fake reader");

        assert_eq!(domain, "recovered-box.example", "must use the BOX domain");
        assert_eq!(
            seed_hex,
            hex::encode(custodied_seed),
            "must install the custodied seed, not a fresh one"
        );
    }

    /// Normal provisioning still mints a fresh seed + uses the admin's handle
    /// domain (the pre-leg-3 behaviour, preserved).
    #[tokio::test]
    async fn resolve_normal_mode_generates_seed_and_uses_handle_domain() {
        let m = OnboardingMachine::new(CountingObserver::new());
        {
            let mut s = m.state.lock().unwrap();
            s.recovery_intent = false;
            s.handle = "alice@example.com".into();
        }
        let snap = m.snapshot();
        let (domain, seed_hex) = m
            .resolve_deployment_seed_and_domain(&snap)
            .await
            .expect("normal resolution should succeed");

        assert_eq!(domain, "example.com", "normal mode uses the handle domain");
        assert_eq!(seed_hex.len(), 64, "a fresh 64-hex Ed25519 seed");
        assert!(hex::decode(&seed_hex).is_ok(), "seed must be valid hex");
    }

    /// A recovery config-read **failure** (connect / auth / decode error, or a
    /// reachable nest whose custody holds no seed for the box) surfaces as a
    /// loud `ProvisioningFailed` — recovery-mode provisioning never silently
    /// falls back to minting a *fresh* identity. Both target production readers
    /// now do a real authed custody read (wasm via `run_silent_challenge` +
    /// browser WebPKI, native via `mint_bearer_over_handshake` + a pinned
    /// `TokenNestClient`); this asserts the machine's error mapping with a fake
    /// reader that fails, so it stays deterministic and touches no network.
    #[tokio::test]
    async fn resolve_recovery_mode_reader_error_surfaces_as_provisioning_failed() {
        #[derive(Debug)]
        struct FailingRecoveryReader;

        #[async_trait::async_trait]
        impl crate::recovery_config::RecoveryConfigReader for FailingRecoveryReader {
            async fn resolve(
                &self,
                _nest_url: Option<&str>,
                _owner_secret_hex: &str,
                _nest_actor_id_hex: &str,
            ) -> Result<
                crate::recovery_config::RecoveryProvisionInput,
                crate::recovery_config::RecoveryConfigError,
            > {
                Err(crate::recovery_config::RecoveryConfigError::Failed(
                    "simulated custody read failure".into(),
                ))
            }
        }

        let m = OnboardingMachine::new(CountingObserver::new());
        {
            let mut s = m.state.lock().unwrap();
            s.recovery_intent = true;
            s.recovery_selected_nest_id = Some("ab".repeat(32));
            s.imported_secret = Some("cd".repeat(32).into());
            s.nest_url = "https://surviving.example".into();
        }
        *m.recovery_config_reader.write().unwrap() = Arc::new(FailingRecoveryReader);

        let snap = m.snapshot();
        let err = m
            .resolve_deployment_seed_and_domain(&snap)
            .await
            .expect_err("a reader failure must surface, not silently succeed");
        assert!(
            matches!(err, OnboardingError::ProvisioningFailed { .. }),
            "reader error should surface a ProvisioningFailed, got {err:?}"
        );
    }

    /// A box selected for recovery that turns out to have no domain (a domainless
    /// private home-relay box) can't be cloud-re-provisioned → a clear error
    /// (the UI routes such a box to self-hosted recovery).
    #[tokio::test]
    async fn resolve_recovery_mode_domainless_box_errors() {
        let m = OnboardingMachine::new(CountingObserver::new());
        {
            let mut s = m.state.lock().unwrap();
            s.recovery_intent = true;
            s.recovery_selected_nest_id = Some("ab".repeat(32));
            s.imported_secret = Some("cd".repeat(32).into());
            s.nest_url = "https://surviving.example".into();
        }
        *m.recovery_config_reader.write().unwrap() = Arc::new(FakeRecoveryReader {
            seed: [0x22u8; 32],
            domain: None,
        });
        let snap = m.snapshot();
        let err = m
            .resolve_deployment_seed_and_domain(&snap)
            .await
            .expect_err("a domainless box can't be cloud-re-provisioned");
        assert!(
            matches!(err, OnboardingError::InvalidTransition { .. }),
            "domainless box should surface InvalidTransition, got {err:?}"
        );
    }

    /// The **whole recovery drive**, end to end against a faked cloud provider:
    /// the custodied seed `resolve_deployment_seed_and_domain` resolves must
    /// actually reach the VPS provider's create call, inside the cloud-init
    /// user-data, as `FAUNA_DEPLOYMENT_SEED`.
    ///
    /// Why this exists even though every link already has a test. The four
    /// tests above stop at `resolve_deployment_seed_and_domain`'s *return
    /// value*, and `cloud_init::tests` asserts `build_cloud_init` renders a
    /// seed it is *handed*. Nothing asserted the links **between** them —
    /// resolve → `CloudInitParams.deployment_seed` → `build_cloud_init` →
    /// `create_server(user_data)`. A refactor that dropped the field on the
    /// recovery path (or re-minted a fresh seed after resolving) would leave
    /// every existing test green while silently rebuilding the box under a
    /// **different** `nest_actor_id` — the exact trust break box-recovery
    /// exists to prevent (`box-recovery.md` § Goal assertion (1)).
    ///
    /// This is the "cloud provider's API-drive e2e" that doc long recorded as
    /// blocked on a mock-provisioner seam. The seam was already there: every
    /// provider adapter has `with_base_url`, `dispatch::{vps,dns}_provider`
    /// take an `override_base_url`, and the machine reads it per call via
    /// `provider_base_url` — the same channel `tests/e2e-unified/fakes/
    /// fake_cloud.py` drives. Here the fake is three `wiremock` servers, so the
    /// assertion stays in-process, deterministic, and latency-independent
    /// (testing.md convention 14 — no wall-clock waits: the drive is awaited,
    /// then the recorded request is read).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recovery_drive_sends_the_custodied_seed_to_the_vps_create_call() {
        use fauna_provisioning::vps::{ServerTypeInfo, VpsLocation};
        use serde_json::json;
        use wiremock::matchers::{method, path, path_regex, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The box being recovered: its custodied seed + its own domain (NOT the
        // admin's handle domain, which is deliberately different below). The
        // selected box's identity is the seed's OWN derived public key — row
        // 496's fix refuses the run when these two disagree (a corrupt custody
        // entry), so a real drive needs them consistent; the mismatch case gets
        // its own dedicated test below.
        let custodied_seed = [0x5au8; 32];
        let box_domain = "recovered-box.example";
        let box_id = ed25519_dalek::SigningKey::from_bytes(&custodied_seed)
            .verifying_key()
            .to_bytes();
        let box_id_hex = hex::encode(box_id);

        // ── The faked provider surface ──────────────────────────────────────
        // Split across three servers so a stray call can't be absorbed by
        // another backend's catch-all (one server per `provider_base_urls` key).
        let vps_mock = MockServer::start().await;
        let dns_mock = MockServer::start().await;
        let nest_mock = MockServer::start().await;

        // VPS: no pre-existing server (so the create actually runs), then the
        // create itself, then the rDNS pair the Dns step drives.
        Mock::given(method("GET"))
            .and(path("/servers"))
            .and(query_param("name", box_domain.replace('.', "-")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"servers": []})))
            .mount(&vps_mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/servers"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "server": {"id": 12345, "public_net": {"ipv4": {"ip": "203.0.113.5"}}}
            })))
            .mount(&vps_mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/servers/12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "server": {
                    "id": 12345,
                    "public_net": {"ipv4": {"ip": "203.0.113.5", "dns_ptr": null}}
                }
            })))
            .mount(&vps_mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/servers/12345/actions/change_dns_ptr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "action": {"id": 1, "status": "success"}
            })))
            .mount(&vps_mock)
            .await;

        // DNS (Cloudflare shape): a zone whose name IS the box's domain, so the
        // Domain step pre-flights to `Skipped(ZoneAlreadyVerified)`. Records
        // list empty so every create is a real create.
        Mock::given(method("GET"))
            .and(path("/zones"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true, "errors": [], "messages": [],
                "result": [{"id": "zone-abc", "name": box_domain, "status": "active"}]
            })))
            .mount(&dns_mock)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/zones/[^/]+/dns_records$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true, "errors": [], "messages": [], "result": []
            })))
            .mount(&dns_mock)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/zones/[^/]+/dns_records$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true, "errors": [], "messages": [], "result": {"id": "rec-001"}
            })))
            .mount(&dns_mock)
            .await;

        // Nest: the Online step's liveness poll, up on the first try.
        Mock::given(method("GET"))
            .and(path("/api/v1/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
            .mount(&nest_mock)
            .await;

        let m = OnboardingMachine::new_with_provider_base_urls(
            CountingObserver::new(),
            Some(HashMap::from([
                ("vps".to_string(), vps_mock.uri()),
                ("dns".to_string(), dns_mock.uri()),
                ("nest".to_string(), nest_mock.uri()),
            ])),
        );
        *m.recovery_config_reader.write().unwrap() = Arc::new(FakeRecoveryReader {
            seed: custodied_seed,
            domain: Some(box_domain.to_string()),
        });

        m.mutate(|s| {
            // Recovery intent + the selected lost box + the imported identity
            // and the surviving nest the reader would read the custody from.
            s.recovery_intent = true;
            s.recovery_selected_nest_id = Some(box_id_hex.clone());
            s.imported_secret = Some("cd".repeat(32).into());
            s.nest_url = "https://surviving.example".into();
            // A *different* handle domain, so an assertion on the box's own
            // domain can't pass by coincidence.
            s.handle = "admin@my-handle.example".into();

            s.vps.selected_provider_id = Some("hetzner".into());
            s.vps.creds = HashMap::from([("api-token".to_string(), "tkn".to_string().into())]);
            s.vps.server_types = vec![ServerTypeInfo {
                id: "cx23".into(),
                vcpu: 2,
                mem_gb: 4.0,
                disk_gb: 40,
                price_monthly_cents: 451,
                currency: "EUR".into(),
            }];
            s.vps.selected_server_type_id = Some("cx23".into());
            s.vps.locations = vec![VpsLocation {
                id: "fsn1".into(),
                name: "Falkenstein".into(),
                city: "Falkenstein".into(),
                country: "DE".into(),
            }];
            s.vps.selected_location_id = Some("fsn1".into());
            // Pin the mail intent so the derivation never probes the domain.
            s.vps.enable_mail = Some(false);

            s.dns.set_up_later = false;
            s.dns.buy_domain = false;
            s.dns.selected_provider_id = Some("cloudflare".into());
            s.dns.creds = HashMap::from([("api-token".to_string(), "tkn".to_string().into())]);
            s.dns.zone_id = Some("zone-abc".into());
        });

        run_provisioning_inner(m.clone())
            .await
            .expect("the recovery drive should reach Provisioned against the faked provider");

        // ── The assertion this test exists for ──────────────────────────────
        let creates: Vec<_> = vps_mock
            .received_requests()
            .await
            .expect("wiremock records requests")
            .into_iter()
            .filter(|r| r.method == "POST" && r.url.path() == "/servers")
            .collect();
        assert_eq!(
            creates.len(),
            1,
            "the drive should create exactly one server, saw {}",
            creates.len()
        );
        let body: serde_json::Value =
            serde_json::from_slice(&creates[0].body).expect("create body is JSON");
        let user_data = body["user_data"]
            .as_str()
            .expect("the create call must carry cloud-init user_data");

        assert!(
            user_data.contains(&format!(
                "FAUNA_DEPLOYMENT_SEED: {}",
                hex::encode(custodied_seed)
            )),
            "the rebuilt box must be provisioned with the CUSTODIED seed — without it the box \
             mints a fresh identity and every TOFU-pinned client rejects it (box-recovery.md \
             § Goal assertion (1)). user_data:\n{user_data}"
        );
        assert_eq!(
            body["name"].as_str(),
            Some(box_domain.replace('.', "-").as_str()),
            "the server is named after the BOX's own domain, not the admin's handle domain"
        );
        assert_eq!(
            *m.nest_expected_identity.read().unwrap(),
            Some((box_domain.to_string(), box_id)),
            "row 496: the recovery path must hold the SELECTED box's identity \
             (recovery_selected_nest_id) directly, not merely re-derive it from \
             the seed"
        );
    }

    /// A corrupt custody entry —
    /// the custodied seed no longer derives to the selected box's own
    /// `nest_actor_id` — must refuse the re-provision before `create_server`,
    /// never silently fall through to `hold_first_contact_root`'s mint-path
    /// malformed-seed tolerance (which holds nothing and downgrades a
    /// no-TOFU-window recovery to TOFU-on-host).
    #[tokio::test]
    async fn recovery_refuses_when_custodied_seed_does_not_match_selected_box_identity() {
        use fauna_provisioning::vps::ServerTypeInfo;

        let custodied_seed = [0x5au8; 32];
        let box_domain = "recovered-box.example";
        // Deliberately UNRELATED to `custodied_seed`'s derived identity — the
        // shape of a corrupted custody entry.
        let wrong_box_id_hex = "ab".repeat(32);

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        *m.recovery_config_reader.write().unwrap() = Arc::new(FakeRecoveryReader {
            seed: custodied_seed,
            domain: Some(box_domain.to_string()),
        });
        m.mutate(|s| {
            s.recovery_intent = true;
            s.recovery_selected_nest_id = Some(wrong_box_id_hex);
            s.imported_secret = Some("cd".repeat(32).into());
            s.nest_url = "https://surviving.example".into();
            s.handle = "admin@my-handle.example".into();
            // Enough VPS selection to clear `run_provisioning_inner`'s earlier
            // server-type lookup — this test is about the LATER trust-root
            // refusal, so the run must reach it rather than erroring earlier.
            s.vps.server_types = vec![ServerTypeInfo {
                id: "cx23".into(),
                vcpu: 2,
                mem_gb: 4.0,
                disk_gb: 40,
                price_monthly_cents: 451,
                currency: "EUR".into(),
            }];
            s.vps.selected_server_type_id = Some("cx23".into());
        });

        let err = run_provisioning_inner(m.clone())
            .await
            .expect_err("a custody mismatch must refuse the run, not proceed");
        assert!(
            matches!(err, OnboardingError::ProvisioningFailed { .. }),
            "expected ProvisioningFailed (the pending-slot refusal's own error \
             shape), got {err:?}"
        );
        assert_eq!(
            *m.nest_expected_identity.read().unwrap(),
            None,
            "a refused recovery must not have pinned any first-contact root"
        );
    }

    /// Regression test for the deferred-DNS path: `stash_provisioning_result`
    /// must write `nest_url` when the result is `Ok(DnsPostInstructions)`.
    /// Before the fix, this arm only cleared `error_message`, leaving
    /// `nest_url` empty; `continue_from_dns_post_instructions` then surfaced
    /// an empty URL in `WizardOutcome::AwaitingManualDns`.
    /// `derive_pubkey` produces the Ed25519 public key matching the secret,
    /// and `sign_bytes` produces a signature that verifies under that key.
    /// Round-trips a known-good signature through `ed25519-dalek::Verifier`
    /// to confirm the helpers are wire-compatible with the nest's
    /// `verify_sig` (same crate, same canonical message convention).
    #[test]
    fn ed25519_helpers_round_trip() {
        // verify-ok(test): this module signs with a locally generated key and checks
        // its own signature back — no wire-supplied key reaches it, so the permissive
        // trait is harmless here. Production verification goes through
        // `fauna_core::identity::verify_detached`; the walk guard
        // `fauna-core/tests/one_ed25519_verification_shape.rs` reads this marker.
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};

        // 32-byte all-1 secret — deterministic for this test.
        let secret_hex = "01".repeat(32);
        let pubkey_hex = derive_pubkey(&secret_hex);
        assert_eq!(pubkey_hex.len(), 64, "pubkey must be 32 bytes hex-encoded");

        let pubkey_bytes: [u8; 32] = hex::decode(&pubkey_hex).unwrap().try_into().unwrap();
        let vk = VerifyingKey::from_bytes(&pubkey_bytes).unwrap();

        let msg = b"alice@example.testhandle12345";
        let sig_hex = sign_bytes(&secret_hex, msg);
        assert_eq!(sig_hex.len(), 128, "sig must be 64 bytes hex-encoded");

        let sig_bytes = hex::decode(&sig_hex).unwrap();
        let sig = Signature::from_slice(&sig_bytes).unwrap();
        vk.verify(msg, &sig)
            .expect("signature must verify under the derived pubkey");

        // Wrong message must fail.
        assert!(vk.verify(b"different message", &sig).is_err());

        // Bad secret hex returns empty strings (graceful).
        assert_eq!(derive_pubkey("not hex"), "");
        assert_eq!(sign_bytes("not hex", msg), "");
    }

    /// The machine signs with the same single-source `fauna_protocol` builders
    /// the nest verifies with (the finding: tagged + length-prefixed for
    /// the variable-length contexts). Pin the tag + prefix structure so a
    /// regression to hand-rolled bytes is caught here.
    #[test]
    fn signed_message_builders_match_nest_canonical_form() {
        let actor: [u8; 32] = [0xAB; 32];
        let ts: u64 = 0x0123456789ABCDEF;

        let invite_msg =
            fauna_protocol::invite::invite_submit_signed_message(&actor, "alice", "msg", ts);
        let tag = fauna_protocol::sig_domain::INVITE_SUBMIT_V1;
        // Length-prefixed element list, tag first.
        assert_eq!(&invite_msg[..8], &(tag.len() as u64).to_be_bytes());
        assert_eq!(&invite_msg[8..8 + tag.len()], tag);

        let register_msg =
            fauna_protocol::account::register_signed_message(&actor, "alice", "example.test", ts);
        let rtag = fauna_protocol::sig_domain::ACCOUNT_REGISTER_V1;
        assert_eq!(&register_msg[..8], &(rtag.len() as u64).to_be_bytes());
        assert_eq!(&register_msg[8..8 + rtag.len()], rtag);
        // Distinct contexts can never produce the same signed message.
        assert_ne!(invite_msg, register_msg);
    }

    /// `dns_provider_eligible` encodes the same capability rule the
    /// `toggle_buy_domain`/`toggle_same_provider_for_vps` deselect guards use,
    /// so all 7 apps can drive per-provider button sensitivity from one
    /// predicate instead of re-deriving `buy_domain → Registrar` /
    /// `same_provider_for_vps → Vps`. Caps (providers_generated.rs):
    /// Cloudflare=[Dns,Registrar], Porkbun=[Dns,Registrar], Hetzner=[Dns,Vps].
    /// A nest on a SUBDOMAIN of a domain the admin already holds at the DNS
    /// provider (`box.example.com`, zone `example.com`). The handle check
    /// reads such a name as available — it has no delegation of its own — so
    /// the page arrives in buy mode; once the admin unticks that and verifies
    /// the provider, the zone list is the ground truth: the provider HAS the
    /// domain and Continue is live. Before `covering_zone`, only a zone named
    /// exactly like the handle's domain counted, the page said the provider
    /// "can't sell this domain", and the wizard was a dead end.
    #[test]
    fn provider_status_counts_a_zone_that_contains_the_handles_domain() {
        use crate::state::ProviderStatus;
        use fauna_provisioning::dns::DnsZone;
        let zone = |id: &str, name: &str| DnsZone {
            id: id.into(),
            name: name.into(),
        };
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.set_current_handle("admin@box.example.com".into());
        m.set_dns_state_for_test(|d| {
            d.selected_provider_id = Some("hetzner".into());
            d.verified = true;
            d.buy_domain = false;
            d.current_zones = vec![zone("z-other", "other.org"), zone("z-apex", "Example.com.")];
        });
        assert!(matches!(
            m.provider_status(),
            ProviderStatus::ProviderHasDomain
        ));
        assert!(
            m.can_continue_dns(),
            "the covering zone makes Continue live"
        );

        // A zone that merely shares a suffix STRING is not a parent.
        m.set_dns_state_for_test(|d| d.current_zones = vec![zone("z", "ample.com")]);
        assert!(!matches!(
            m.provider_status(),
            ProviderStatus::ProviderHasDomain
        ));
        // Nor is a child zone a parent of the domain above it.
        m.set_dns_state_for_test(|d| d.current_zones = vec![zone("z", "deep.box.example.com")]);
        assert!(!matches!(
            m.provider_status(),
            ProviderStatus::ProviderHasDomain
        ));
    }

    /// The zone the wizard publishes into is the LONGEST one holding the
    /// domain — never "the first zone on the account".
    #[test]
    fn covering_zone_is_the_longest_zone_holding_the_domain() {
        use fauna_provisioning::dns::DnsZone;
        let zones: Vec<DnsZone> = [
            ("z1", "other.org"),
            ("z2", "example.com"),
            ("z3", "eu.example.com"),
        ]
        .into_iter()
        .map(|(id, name)| DnsZone {
            id: id.into(),
            name: name.into(),
        })
        .collect();
        let id = |d: &str| covering_zone(d, &zones).map(|z| z.id.as_str());
        assert_eq!(id("example.com"), Some("z2"));
        assert_eq!(id("box.example.com"), Some("z2"));
        assert_eq!(id("box.eu.example.com"), Some("z3"));
        assert_eq!(id("BOX.EU.Example.COM."), Some("z3"));
        assert_eq!(id("notexample.com"), None);
        assert_eq!(id("com"), None);
    }

    #[test]
    fn dns_provider_eligible_tracks_buy_domain_and_same_vps() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());

        // No constraints: every DNS provider is eligible. (same_provider_for_vps
        // defaults to true, so clear it for the unconstrained baseline.)
        m.set_dns_state_for_test(|d| d.same_provider_for_vps = false);
        assert!(m.dns_provider_eligible("cloudflare".into()));
        assert!(m.dns_provider_eligible("porkbun".into()));
        assert!(m.dns_provider_eligible("hetzner".into()));
        // Unknown provider id is never eligible.
        assert!(!m.dns_provider_eligible("nope".into()));

        // buy_domain requires the Registrar capability.
        m.set_dns_state_for_test(|d| d.buy_domain = true);
        assert!(
            m.dns_provider_eligible("cloudflare".into()),
            "has Registrar"
        );
        assert!(m.dns_provider_eligible("porkbun".into()), "has Registrar");
        assert!(!m.dns_provider_eligible("hetzner".into()), "no Registrar");

        // same_provider_for_vps requires the Vps capability (independently).
        m.set_dns_state_for_test(|d| {
            d.buy_domain = false;
            d.same_provider_for_vps = true;
        });
        assert!(!m.dns_provider_eligible("cloudflare".into()), "no Vps");
        assert!(!m.dns_provider_eligible("porkbun".into()), "no Vps");
        assert!(m.dns_provider_eligible("hetzner".into()), "has Vps");
    }

    /// A Registrar-kinded field (cloudflare's `account-id`, `kinds:
    /// [registrar]`) must render once buy_domain selects cloudflare as the
    /// registrar — before this fix `visible_dns_fields` only ever checked
    /// Dns/Vps kinds, so a registrar-only field never rendered and its
    /// required credential could never be collected
    /// (`test_registrar_provider_fields_visible[web-cloudflare]`, an
    /// e2e-unified GUI test, caught this on web first).
    #[test]
    fn visible_dns_fields_includes_registrar_kind_when_buying_domain() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.set_dns_state_for_test(|d| {
            d.selected_provider_id = Some("cloudflare".into());
            d.same_provider_for_vps = false;
        });
        let ids = |m: &Arc<OnboardingMachine>| {
            m.visible_dns_fields()
                .into_iter()
                .map(|f| f.id)
                .collect::<Vec<_>>()
        };

        // buy_domain off: only the Dns-kinded field (api-token) shows.
        let before = ids(&m);
        assert!(before.contains(&"api-token".to_string()));
        assert!(!before.contains(&"account-id".to_string()));

        // buy_domain on + cloudflare has Registrar cap: account-id joins.
        m.set_dns_state_for_test(|d| d.buy_domain = true);
        let after = ids(&m);
        assert!(after.contains(&"api-token".to_string()));
        assert!(
            after.contains(&"account-id".to_string()),
            "Registrar-kinded field must be visible once buy_domain selects a Registrar-capable provider"
        );
    }

    /// A disabled control owes the user an on-screen reason (`ui/README.md`
    /// § Copy comprehensibility rule 5), and the eligibility rule lives in this
    /// machine — so the *reason* must come from here too rather than being
    /// re-derived by seven shells. `dns_provider_ineligible_reason` is the
    /// answer's other half: `None` exactly when `dns_provider_eligible` is
    /// true, else the i18n key naming which of the two constraints closed the
    /// row (and therefore which checkbox re-opens it).
    #[test]
    fn dns_provider_ineligible_reason_names_the_closing_constraint() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        let reason = |id: &str| {
            m.dns_provider_ineligible_reason(id.to_string())
                .map(|t| t.key)
        };

        // Unconstrained: nothing is disabled, so nothing owes a reason.
        m.set_dns_state_for_test(|d| d.same_provider_for_vps = false);
        assert_eq!(reason("cloudflare"), None);
        assert_eq!(reason("hetzner"), None);

        // buy_domain closes the non-registrars: hetzner is [Dns, Vps].
        m.set_dns_state_for_test(|d| d.buy_domain = true);
        assert_eq!(reason("cloudflare"), None, "has Registrar");
        assert_eq!(
            reason("hetzner").as_deref(),
            Some("onboarding.dns_config.ineligible_needs_registrar"),
            "no Registrar, and the buy-domain box is what re-opens it"
        );

        // same_provider_for_vps closes the registrars, independently.
        m.set_dns_state_for_test(|d| {
            d.buy_domain = false;
            d.same_provider_for_vps = true;
        });
        assert_eq!(
            reason("cloudflare").as_deref(),
            Some("onboarding.dns_config.ineligible_needs_vps"),
            "no Vps, and the same-provider box is what re-opens it"
        );
        assert_eq!(reason("hetzner"), None, "has Vps");

        // Both boxes on: today every DNS provider carries exactly one of the
        // two capabilities, so each row still names its single blocker — and
        // the whole list is dead, which is precisely why saying why matters.
        m.set_dns_state_for_test(|d| d.buy_domain = true);
        assert_eq!(
            reason("cloudflare").as_deref(),
            Some("onboarding.dns_config.ineligible_needs_vps")
        );
        assert_eq!(
            reason("hetzner").as_deref(),
            Some("onboarding.dns_config.ineligible_needs_registrar")
        );

        // An id with no row on screen has no reason to paint — there is no
        // control to explain. (It is still ineligible; the two answers are
        // deliberately not each other's negation for ids outside PROVIDERS.)
        assert_eq!(reason("nope"), None);
        assert!(!m.dns_provider_eligible("nope".into()));
    }

    /// `dns_status_text_key`'s `NotReady` arm returned an EMPTY key, so at
    /// `dns_config` entry the page painted a blank status line beside a
    /// disabled Continue — a rule-5 breach (`ui/README.md` § Copy
    /// comprehensibility: every disabled control the user can see has an
    /// on-screen reason within eyeshot) on all 7 apps at once, since every app
    /// renders `dns-status-text` from this one getter.
    ///
    /// `NotReady` covers TWO conditions — nothing picked yet, and picked but
    /// not verified — and rule 5's Q2 is explicit that two different conditions
    /// deserve two different messages (the `error_no_set` / `error_no_sync_set`
    /// precedent). So the arm splits rather than sharing one generic string.
    #[test]
    fn not_ready_says_which_step_is_missing_instead_of_nothing() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        let key = || m.dns_status_text_key().key;

        // Entry state: no provider chosen. Continue is dead; say why.
        assert!(!m.can_continue_dns(), "precondition: Continue is disabled");
        assert_eq!(key(), "onboarding.dns_config.status_pick_provider");

        // Chosen but not verified — a different condition, a different
        // message: the user's next act is Verify, not picking again.
        m.select_dns_provider("cloudflare".into());
        assert!(!m.can_continue_dns(), "still disabled until verify lands");
        assert_eq!(key(), "onboarding.dns_config.status_verify_credentials");
    }

    /// The message choice is a total function of the two shortfalls, so the
    /// combination no shipped provider reaches today (neither Registrar nor
    /// Vps) is still covered — `providers.yaml` is generated data that grows.
    #[test]
    fn ineligibility_key_is_total_over_both_shortfalls() {
        assert_eq!(ineligibility_key(false, false), None);
        assert_eq!(
            ineligibility_key(true, false),
            Some("onboarding.dns_config.ineligible_needs_registrar")
        );
        assert_eq!(
            ineligibility_key(false, true),
            Some("onboarding.dns_config.ineligible_needs_vps")
        );
        assert_eq!(
            ineligibility_key(true, true),
            Some("onboarding.dns_config.ineligible_needs_registrar_and_vps")
        );
    }

    /// `should_show_registrar_notes` is true only on the buy-domain path with
    /// a selected provider that declares a `registrar_notes_key` (Porkbun,
    /// Cloudflare; Gandi has none, per providers_generated.rs).
    #[test]
    fn should_show_registrar_notes_only_on_buy_with_notes_provider() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());

        // No provider / not buying → hidden.
        assert!(!m.should_show_registrar_notes());
        m.set_dns_state_for_test(|d| d.selected_provider_id = Some("porkbun".into()));
        assert!(
            !m.should_show_registrar_notes(),
            "notes hidden until buy_domain"
        );

        // Buying + Porkbun (has notes key) → shown.
        m.set_dns_state_for_test(|d| d.buy_domain = true);
        assert!(m.should_show_registrar_notes());

        // Buying + Gandi (no notes key) → hidden.
        m.set_dns_state_for_test(|d| d.selected_provider_id = Some("gandi".into()));
        assert!(!m.should_show_registrar_notes(), "gandi has no notes key");
        // Buying + Cloudflare (has notes key) → shown.
        m.set_dns_state_for_test(|d| d.selected_provider_id = Some("cloudflare".into()));
        assert!(m.should_show_registrar_notes(), "cloudflare has notes key");
    }

    /// `should_show_contact_form` is true only when the selected registrar
    /// requires a WHOIS contact AND `provider_status()` is
    /// `UnregisteredBuyable` (the buy-domain path actually runs). Gandi
    /// requires contact; Porkbun does not.
    #[test]
    fn should_show_contact_form_gated_on_requires_contact_and_buyable() {
        use crate::state::ProviderStatus;
        use fauna_provisioning::registrar::RegistrarAvailability;

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        // Domain unknown to any zone, registrar quoted Buyable, verified.
        m.set_current_handle("alice@example.test".into());

        // Not verified yet → status NotReady → hidden.
        m.set_dns_state_for_test(|d| {
            d.selected_provider_id = Some("gandi".into());
            d.creds.insert(
                "personal-access-token".into(),
                SecretString::from("tok".to_string()),
            );
        });
        assert!(
            !m.should_show_contact_form(),
            "NotReady status hides the form"
        );

        // Verified + Buyable + Gandi (requires contact) → shown.
        m.set_dns_state_for_test(|d| {
            d.verified = true;
            d.current_availability = Some(RegistrarAvailability::Buyable {
                price_cents: 1200,
                currency: Some("USD".into()),
                renewal_cents: None,
            });
        });
        assert!(matches!(
            m.provider_status(),
            ProviderStatus::UnregisteredBuyable { .. }
        ));
        assert!(m.should_show_contact_form(), "gandi requires contact");

        // Same buyable state but Porkbun (no contact required) → hidden.
        m.set_dns_state_for_test(|d| {
            d.selected_provider_id = Some("porkbun".into());
            d.creds.clear();
            d.creds
                .insert("api-key".into(), SecretString::from("k".to_string()));
            d.creds
                .insert("secret-api-key".into(), SecretString::from("s".to_string()));
        });
        assert!(
            !m.should_show_contact_form(),
            "porkbun does not require contact"
        );
    }

    #[test]
    fn a_nest_hint_prefills_the_domain_part_and_nothing_else() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        let step = m.step();
        assert!(m.set_nest_hint(" example.org ".into()));
        assert_eq!(m.current_handle(), "@example.org");
        // A prefill, not a navigation: the page stays where it was.
        assert_eq!(m.step(), step);

        // A local part already typed survives; the domain part is replaced.
        m.set_current_handle("alice@other.test".into());
        assert!(m.set_nest_hint("example.org".into()));
        assert_eq!(m.current_handle(), "alice@example.org");

        // Direct nest addresses classify too, with or without a port.
        m.set_current_handle(String::new());
        assert!(m.set_nest_hint("192.168.1.50:8443".into()));
        assert_eq!(m.current_handle(), "@192.168.1.50:8443");
        assert!(m.set_nest_hint("pi.local".into()));
        assert_eq!(m.current_handle(), "@pi.local");
    }

    #[test]
    fn an_unclassifiable_nest_hint_is_dropped_silently() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.set_current_handle("alice".into());
        for junk in [
            "",
            "   ",
            "evil.example/path",
            "user@evil.example",
            "evil.example?x=1",
            "two words.example",
            "host:notaport",
            "<script>",
        ] {
            assert!(!m.set_nest_hint(junk.into()), "{junk:?} must not classify");
            assert_eq!(m.current_handle(), "alice", "{junk:?} left a trace");
        }
        assert_eq!(m.error_message(), None, "a dropped hint is never an error");
    }

    #[test]
    fn navigate_to_claim_code_with_code_pre_loads_prefill() {
        let obs = CountingObserver::new();
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(obs);

        // The factory-reset re-onboard path: land on ClaimCode with the
        // returned code carried so the page can pre-fill its input.
        m.navigate_to_claim_code_for_known_nest_with_code(
            "https://nest.example".into(),
            "alice@example.test".into(),
            "abc123".into(),
        );
        assert_eq!(m.step(), OnboardingStep::ClaimCode);
        assert_eq!(m.nest_url(), "https://nest.example");
        assert_eq!(m.claim_code_prefill().as_deref(), Some("abc123"));

        // The ordinary unclaimed-nest path clears the prefill (the human types
        // the admin's printed code), so a later plain navigate wipes it.
        m.navigate_to_claim_code_for_known_nest(
            "https://nest.example".into(),
            "alice@example.test".into(),
        );
        assert_eq!(m.step(), OnboardingStep::ClaimCode);
        assert_eq!(m.claim_code_prefill(), None);
    }

    #[test]
    fn stash_provisioning_result_sets_nest_url_on_deferred_dns_path() {
        let obs = CountingObserver::new();
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(obs);
        m.set_current_handle("alice@example.test".into());

        // Simulate successful deferred-DNS provisioning.
        stash_provisioning_result(m.clone(), Ok(NextStep::DnsPostInstructions));

        assert_eq!(
            m.nest_url(),
            "https://example.test",
            "stash_provisioning_result must populate nest_url on DnsPostInstructions path"
        );
    }

    #[test]
    fn stash_provisioning_result_captures_reach_by_ip_override_and_reset_clears_it() {
        let obs = CountingObserver::new();
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(obs);
        m.set_current_handle("alice@example.test".into());

        // Simulate the orchestrator having already stashed a successful result
        // (real code does this via `fauna_provisioning::progress::set_run_succeeded`
        // before `run_provisioning_inner` returns, in all three provisioning paths).
        m.provisioning.lock().unwrap().result =
            Some(fauna_provisioning::progress::ProvisionResultPlain {
                server_id: "srv-1".into(),
                ipv4: "203.0.113.9".into(),
                domain: "example.test".into(),
                claim_code: "CODE".into(),
            });

        stash_provisioning_result(
            m.clone(),
            Ok(NextStep::Provisioned {
                url: "https://example.test".into(),
            }),
        );

        assert_eq!(
            *m.nest_resolve_override.read().unwrap(),
            Some(("example.test".to_string(), "203.0.113.9".parse().unwrap())),
            "a successful provisioning result must capture (domain, ip) for WsNestApi's reach-by-IP override"
        );

        m.reset();
        assert_eq!(
            *m.nest_resolve_override.read().unwrap(),
            None,
            "reset() must drop a stale reach-by-IP override"
        );
    }

    /// Track 2 — the injected seed's derived public identity is held as the
    /// first-contact identity root for the provisioned domain, and dropped on
    /// reset (a re-provision mints a FRESH seed; a stale root would hard-fail
    /// the new box's genuine identity).
    #[test]
    fn hold_first_contact_root_derives_public_identity_and_reset_clears_it() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());

        let seed = [7u8; 32];
        m.hold_first_contact_root("example.test", &hex::encode(seed));

        let expected = ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes();
        assert_eq!(
            *m.nest_expected_identity.read().unwrap(),
            Some(("example.test".to_string(), expected)),
            "the held root is the seed's DERIVED PUBLIC key, keyed by the provisioned domain"
        );

        m.reset();
        assert_eq!(
            *m.nest_expected_identity.read().unwrap(),
            None,
            "reset() must drop a stale first-contact identity root"
        );
    }

    /// The exit races a claim's own nest-binding write if taken mid-claim, so the
    /// machine refuses it there (and the surface disables the button off the same
    /// getter) — and takes it again the moment the state is anything else.
    #[test]
    fn abandon_awaiting_manual_dns_is_refused_only_while_a_claim_is_in_flight() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.seed_identity("01".repeat(32));
        m.seed_awaiting_manual_dns(
            "https://box.example.test".into(),
            "alice@box.example.test".into(),
            Vec::new(),
            "K7Q2-M9XJ".into(),
        );
        m.set_awaiting_dns_state(
            AwaitingDnsState::Claiming,
            "onboarding.awaiting_dns.claiming",
        );
        assert!(!m.awaiting_dns_fallthrough_enabled());

        m.abandon_awaiting_manual_dns();

        assert!(
            matches!(
                m.wizard_outcome(),
                Some(WizardOutcome::AwaitingManualDns { .. })
            ),
            "a claim in flight must not have the surface pulled out from under it"
        );
        assert_eq!(m.step(), OnboardingStep::Done);

        m.set_awaiting_dns_state(
            AwaitingDnsState::Checking,
            "onboarding.awaiting_dns.checking",
        );
        assert!(m.awaiting_dns_fallthrough_enabled());
        m.abandon_awaiting_manual_dns();
        assert_eq!(m.wizard_outcome(), None);
        assert_eq!(m.step(), OnboardingStep::HandleEntry);
    }

    /// The exit is a `reset()` that keeps the identity, and `reset()` keeps the
    /// machine's memory of the box in flight (§ 6 — the box still exists and still
    /// bills) — so choosing the SAME domain again resumes it instead of minting a
    /// second box with a fresh code and identity.
    #[test]
    fn abandon_awaiting_manual_dns_keeps_the_in_memory_pending_provision_row() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.seed_identity("01".repeat(32));
        let record = AwaitingDnsRecord {
            nest_url: "https://box.example.test".into(),
            handle: "alice@box.example.test".into(),
            dns_records_json: String::new(),
            claim_code: "K7Q2-M9XJ".into(),
            reach_ipv4: Some("203.0.113.9".into()),
            nest_actor_id: Some(hex::encode([0x5A_u8; 32])),
        };
        m.seed_awaiting_manual_dns_record(record.clone());

        m.abandon_awaiting_manual_dns();

        assert_eq!(m.wizard_outcome(), None);
        assert_eq!(m.pending_provision_row(), Some(record));
    }

    /// The relaunch door re-holds the identity the box was built with, keyed by
    /// the identity URL's host, and retains the row as the box in flight — so
    /// the "Almost ready" surface's first poll verifies the box it was told
    /// about (no TOFU window across a relaunch), and a "start over" onto the
    /// same domain afterwards resumes that box instead of minting over it.
    #[test]
    fn seed_awaiting_manual_dns_record_holds_the_built_with_identity_and_retains_the_row() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        let id = [0x5A_u8; 32];
        let record = AwaitingDnsRecord {
            nest_url: "https://box.example.test".into(),
            handle: "alice@box.example.test".into(),
            dns_records_json: String::new(),
            claim_code: "K7Q2-M9XJ".into(),
            reach_ipv4: Some("203.0.113.9".into()),
            nest_actor_id: Some(hex::encode(id)),
        };
        m.seed_awaiting_manual_dns_record(record.clone());

        assert_eq!(
            *m.nest_expected_identity.read().unwrap(),
            Some(("box.example.test".to_string(), id)),
            "the slot's built-with identity is held for the identity URL's host"
        );
        assert_eq!(m.pending_provision_row(), Some(record));
        assert!(matches!(
            m.wizard_outcome(),
            Some(WizardOutcome::AwaitingManualDns { .. })
        ));

        // A record from before the field existed pins nothing and still seeds.
        let m2: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m2.seed_awaiting_manual_dns_json(
            "https://box.example.test".into(),
            "alice@box.example.test".into(),
            "[]".into(),
            "K7Q2-M9XJ".into(),
        );
        assert_eq!(*m2.nest_expected_identity.read().unwrap(), None);
        assert_eq!(
            m2.pending_provision_row().map(|r| r.claim_code),
            Some("K7Q2-M9XJ".to_string())
        );
    }

    /// **The reach re-arm** (`onboarding.md` § "Almost ready" surface, *Reach*:
    /// "the poll dials the box at the slot's `reach_ipv4` … native via the
    /// resolve override the seeder re-arms").
    ///
    /// The in-run arming (`install_reach_override`, pinned above) reads the
    /// *run's* result, which a relaunch does not have — so without this the
    /// resumed surface polls by domain and waits out DNS propagation for a box
    /// that has answered on its IP since cloud-init finished. Arming it in the
    /// record-taking seeder is what gives every native app the re-arm at once:
    /// they all hydrate through this door.
    #[test]
    fn seed_awaiting_manual_dns_record_re_arms_the_reach_address() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.seed_awaiting_manual_dns_record(AwaitingDnsRecord {
            nest_url: "https://box.example.test".into(),
            handle: "alice@box.example.test".into(),
            dns_records_json: String::new(),
            claim_code: "K7Q2-M9XJ".into(),
            reach_ipv4: Some("203.0.113.9".into()),
            nest_actor_id: None,
        });

        assert_eq!(
            *m.nest_resolve_override.read().unwrap(),
            Some((
                "box.example.test".to_string(),
                "203.0.113.9".parse().unwrap()
            )),
            "the slot's address is armed against the IDENTITY url's host — the \
             name stays the name (SNI, Host, cert, and the claim's registration \
             are all unaffected); only the socket target moves"
        );
    }

    /// A slot with no address arms nothing and dials `nest_url` as before — the
    /// goal doc's own fallback for a crash between the mint and `create_server`
    /// returning. Same for a
    /// malformed address: never a panic, never a half-armed override.
    #[test]
    fn a_slot_without_a_reach_address_arms_nothing() {
        let record = |ip: Option<&str>| AwaitingDnsRecord {
            nest_url: "https://box.example.test".into(),
            handle: "alice@box.example.test".into(),
            dns_records_json: String::new(),
            claim_code: "K7Q2-M9XJ".into(),
            reach_ipv4: ip.map(str::to_string),
            nest_actor_id: None,
        };

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.seed_awaiting_manual_dns_record(record(None));
        assert_eq!(*m.nest_resolve_override.read().unwrap(), None);

        let m2: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m2.seed_awaiting_manual_dns_record(record(Some("not-an-address")));
        assert_eq!(*m2.nest_resolve_override.read().unwrap(), None);
    }

    /// `reset()` drops the held root (it is keyed by a domain the next run may
    /// change) but NOT the pending-provision row — the slot survives a "start
    /// over" (§ 6) and so must the machine's memory of the box it names.
    #[test]
    fn reset_keeps_the_pending_provision_row_but_drops_the_held_root() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        let record = AwaitingDnsRecord {
            nest_url: "https://box.example.test".into(),
            handle: "alice@box.example.test".into(),
            dns_records_json: String::new(),
            claim_code: "K7Q2-M9XJ".into(),
            reach_ipv4: None,
            nest_actor_id: Some(hex::encode([0x5A_u8; 32])),
        };
        m.seed_awaiting_manual_dns_record(record.clone());
        m.reset();
        assert_eq!(*m.nest_expected_identity.read().unwrap(), None);
        assert_eq!(m.pending_provision_row(), Some(record));
    }

    /// A malformed seed hex writes nothing — mirroring the nest's own
    /// `decode_deployment_seed` tolerance (the box would mint its own identity;
    /// pinning a root derived from garbage would hard-fail its first contact).
    #[test]
    fn hold_first_contact_root_ignores_malformed_seed() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.hold_first_contact_root("example.test", "not-hex");
        m.hold_first_contact_root("example.test", "abcd"); // too short
        assert_eq!(*m.nest_expected_identity.read().unwrap(), None);
    }

    /// Track 2, self-hosted arm: pasting the
    /// console's `fauna://claim` URI pins the printed identity for the claim
    /// host — and the pin is held BEFORE anything is sent, so even a submit
    /// that fails later (no identity imported here, so it fails pre-network)
    /// has already armed the channel verification.
    #[tokio::test]
    async fn a_claim_uri_paste_pins_the_console_identity_for_the_claim_host() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.navigate_to_claim_code_for_known_nest(
            "https://nest.example.test:8443/".into(),
            "alice@example.test".into(),
        );
        let id = [0xAB_u8; 32];
        let uri = fauna_core::claim_code::claim_uri("K7Q2-M9XJ", &hex::encode(id));
        let _ = m.wizard_submit_claim_code(uri).await;
        assert_eq!(
            *m.nest_expected_identity.read().unwrap(),
            Some(("nest.example.test".to_string(), id)),
            "the pasted URI's identity is held, keyed by the claim URL's bare host"
        );
    }

    /// A bare code keeps today's behavior exactly: no pin, no refusal.
    #[tokio::test]
    async fn a_bare_code_submit_pins_nothing() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.navigate_to_claim_code_for_known_nest(
            "https://nest.example.test".into(),
            "alice@example.test".into(),
        );
        let _ = m.wizard_submit_claim_code("K7Q2-M9XJ".into()).await;
        assert_eq!(
            *m.nest_expected_identity.read().unwrap(),
            None,
            "a bare code must not invent a pin"
        );
    }

    /// A URI whose `nest=` is not 64-hex refuses loudly and pins nothing — an
    /// input that LOOKS protected must never silently continue unpinned.
    #[tokio::test]
    async fn a_malformed_claim_uri_is_refused_and_pins_nothing() {
        use crate::snapshots::ClaimCodeState;
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.navigate_to_claim_code_for_known_nest(
            "https://nest.example.test".into(),
            "alice@example.test".into(),
        );
        let _ = m
            .wizard_submit_claim_code("fauna://claim?code=K7Q2-M9XJ&nest=nothex".into())
            .await;
        assert_eq!(*m.nest_expected_identity.read().unwrap(), None);
        let snap = m.claim_code_snapshot();
        assert!(
            matches!(snap.state, ClaimCodeState::Invalid { .. }),
            "the malformed URI must surface as an invalid-input refusal, got {:?}",
            snap.state
        );
    }

    /// The four claim-time serving-enablement intents are **machine-derived**
    /// — there are no checkboxes (`onboarding.md` § 3b: they are retired with
    /// the `encryption_mode_choice` page and are *not* relocated onto
    /// § 3b-bis, which stays confirm-only). Each is ON for a real registerable
    /// domain and OFF for a loopback / IP-literal handle target: mail needs
    /// DNS/MX/DKIM/TLS, and CalDAV/CardDAV/WebDAV each need a publicly-trusted
    /// cert for `mail.<domain>`.
    ///
    /// All four share the one `handle_targets_real_domain` predicate today, but
    /// they stay **separately gated** on the wire (CalDAV needs only the HTTPS
    /// surface, so it can be on where email is off — `caldav-server.md`
    /// § Independent enablement). The admin's change surface after onboarding is
    /// the admin-mail / admin-calendar / admin-contacts / admin-files pages.
    ///
    /// This test isolates the **handle-locality** axis, so it stipulates the
    /// claim axis (`claim_completed`) below rather than driving a claim. The
    /// claim axis has its own coverage — a plain sign-in derives all four OFF
    /// whatever the handle says — in
    /// `tests/serving_enablement_derivation.rs::plain_sign_in_on_a_real_domain_public_box_derives_all_four_off`.
    #[test]
    fn serving_enablement_defaults_follow_handle_domain() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.mutate(|s| s.claim_completed = true);

        let set_handle = |url: &str, handle: &str| {
            m.set_nest_url(url.into());
            m.set_current_handle(handle.into());
        };

        // Real registerable domain → all four ON.
        set_handle("https://example.com", "test@example.com");
        assert!(m.email_enable_requested(), "real domain → email ON");
        assert!(m.caldav_enable_requested(), "real domain → caldav ON");
        assert!(m.carddav_enable_requested(), "real domain → carddav ON");
        assert!(m.webdav_enable_requested(), "real domain → webdav ON");

        // localhost → all four OFF.
        set_handle("http://localhost:3000", "test@localhost");
        assert!(!m.email_enable_requested(), "localhost → email OFF");
        assert!(!m.caldav_enable_requested(), "localhost → caldav OFF");
        assert!(!m.carddav_enable_requested(), "localhost → carddav OFF");
        assert!(!m.webdav_enable_requested(), "localhost → webdav OFF");

        // IP literal → all four OFF.
        set_handle("http://127.0.0.1:3000", "test@127.0.0.1");
        assert!(!m.email_enable_requested(), "IP literal → email OFF");
        assert!(!m.caldav_enable_requested(), "IP literal → caldav OFF");
        assert!(!m.carddav_enable_requested(), "IP literal → carddav OFF");
        assert!(!m.webdav_enable_requested(), "IP literal → webdav OFF");
    }

    /// The `vps-config-mail-mode-toggle` defaults ON for a real registerable
    /// domain (a mail box) and OFF for a loopback / IP-literal handle target,
    /// reusing the same `handle_targets_real_domain` predicate as enable-email.
    /// The user's explicit `set_provision_mail_mode` choice overrides the
    /// derived default. Per `docs/goal/behavior/onboarding.md` §5.
    #[test]
    fn provision_mail_mode_default_follows_handle_then_toggle_overrides() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());

        // Real domain → default ON (mail box).
        m.set_current_handle("test@example.com".into());
        assert!(
            m.provision_mail_mode_enabled(),
            "real domain defaults mail-mode ON"
        );

        // localhost → default OFF (social-only).
        m.set_current_handle("test@localhost".into());
        assert!(
            !m.provision_mail_mode_enabled(),
            "localhost handle defaults mail-mode OFF"
        );

        // Explicit user choice overrides the derived default, regardless of handle.
        m.set_provision_mail_mode(true);
        assert!(
            m.provision_mail_mode_enabled(),
            "Some(true) overrides the localhost default"
        );
        m.set_provision_mail_mode(false);
        m.set_current_handle("test@example.com".into());
        assert!(
            !m.provision_mail_mode_enabled(),
            "Some(false) overrides the real-domain default"
        );
    }

    /// Turning mail ON must drop a previously-selected sub-2 GB plan so the box
    /// is never provisioned mail-on on a plan the RAM gate forbids; a ≥2 GB
    /// selection survives, and turning mail OFF never disturbs a selection.
    /// Per `docs/goal/behavior/onboarding.md` §5 (RAM gate).
    /// The `vps-config-update-channel-row` choice defaults to `stable` and an
    /// explicit choice replaces it — and survives the state's serde round
    /// trip, which is how a wizard resumed mid-flow keeps it.
    #[test]
    fn provision_update_channel_defaults_to_stable_then_follows_the_choice() {
        use fauna_provisioning::cloud_init::UpdateChannel;
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        assert_eq!(m.provision_update_channel(), UpdateChannel::Stable);
        assert_eq!(m.vps_config().update_channel, None, "unset until chosen");

        m.set_provision_update_channel(UpdateChannel::Dev);
        assert_eq!(m.provision_update_channel(), UpdateChannel::Dev);

        let json = serde_json::to_string(&m.vps_config()).unwrap();
        let back: VpsConfigState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.update_channel, Some(UpdateChannel::Dev));
        // A state written before the field existed still loads, as the default.
        let old: VpsConfigState =
            serde_json::from_str(&json.replace(",\"update_channel\":\"dev\"", "")).unwrap();
        assert_eq!(old.update_channel, None);

        m.set_provision_update_channel(UpdateChannel::Stable);
        assert_eq!(m.provision_update_channel(), UpdateChannel::Stable);
    }

    #[test]
    fn set_provision_mail_mode_clears_incompatible_selection() {
        use fauna_provisioning::vps::ServerTypeInfo;
        let st = |id: &str, mem_gb: f32| ServerTypeInfo {
            id: id.into(),
            vcpu: 1,
            mem_gb,
            disk_gb: 25,
            price_monthly_cents: 400,
            currency: "USD".into(),
        };
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.set_vps_state_for_test(|s| {
            s.server_types = vec![st("1gb", 1.0), st("4gb", 4.0)];
            s.selected_server_type_id = Some("1gb".into());
        });

        // Mail OFF (social-only) leaves the 1 GB selection untouched.
        m.set_provision_mail_mode(false);
        assert_eq!(
            m.vps_config().selected_server_type_id.as_deref(),
            Some("1gb"),
            "social-only keeps the 1 GB plan"
        );

        // Mail ON drops the now-too-small selection.
        m.set_provision_mail_mode(true);
        assert_eq!(
            m.vps_config().selected_server_type_id,
            None,
            "enabling mail clears a sub-2 GB plan"
        );

        // A ≥2 GB selection survives enabling mail.
        m.set_vps_state_for_test(|s| s.selected_server_type_id = Some("4gb".into()));
        m.set_provision_mail_mode(true);
        assert_eq!(
            m.vps_config().selected_server_type_id.as_deref(),
            Some("4gb"),
            "a 4 GB plan stays selected with mail on"
        );
    }

    #[test]
    fn provider_base_url_returns_override_when_present() {
        use crate::observer::NullObserver;
        use std::collections::HashMap;
        let mut urls = HashMap::new();
        urls.insert("vps".to_string(), "http://test.example/vps".to_string());
        urls.insert("nest".to_string(), "http://test.example/nest".to_string());
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::new_with_provider_base_urls(observer, Some(urls));
        assert_eq!(
            machine.provider_base_url("vps".into()),
            Some("http://test.example/vps".to_string())
        );
        assert_eq!(
            machine.provider_base_url("nest".into()),
            Some("http://test.example/nest".to_string())
        );
        assert_eq!(machine.provider_base_url("dns".into()), None);
    }

    #[test]
    fn provider_base_url_returns_none_when_no_map() {
        use crate::observer::NullObserver;
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::new(observer);
        assert_eq!(machine.provider_base_url("vps".into()), None);
    }

    /// The runtime `set_provider_base_urls` setter (the native E2E bridge's
    /// in-place twin of web's reconstruct-on-reload) installs an override on a
    /// machine that was built with `None`.
    #[test]
    fn set_provider_base_urls_runtime_setter_round_trips() {
        use crate::observer::NullObserver;
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::new(observer);
        assert_eq!(machine.provider_base_url("dns".into()), None);

        let mut urls = HashMap::new();
        urls.insert("dns".to_string(), "http://127.0.0.1:9/dns".to_string());
        urls.insert("vps".to_string(), "http://127.0.0.1:9/vps".to_string());
        machine.set_provider_base_urls(urls);

        assert_eq!(
            machine.provider_base_url("dns".into()),
            Some("http://127.0.0.1:9/dns".to_string())
        );
        assert_eq!(
            machine.provider_base_url("vps".into()),
            Some("http://127.0.0.1:9/vps".to_string())
        );
    }

    /// The native bridge dispatch: `set_provider_base_urls` applies the
    /// override and `provider_base_url` returns it JSON-encoded — the exact
    /// pair the orchestrator e2e uses to confirm the override reached the
    /// machine before driving a real run.
    #[test]
    fn bridge_set_then_read_provider_base_url() {
        use crate::observer::NullObserver;
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::new(observer);

        let set_ret = machine.call_machine_method_with_result(
            "set_provider_base_urls".into(),
            r#"{"dns":"http://127.0.0.1:9/dns"}"#.into(),
        );
        assert_eq!(set_ret, None, "setter returns no value");

        // `provider_base_url` arg is the JSON-encoded key string.
        let got = machine
            .call_machine_method_with_result("provider_base_url".into(), "\"dns\"".into())
            .expect("reader returns a value");
        assert_eq!(got, "\"http://127.0.0.1:9/dns\"");

        // Missing key serializes to JSON null.
        let missing = machine
            .call_machine_method_with_result("provider_base_url".into(), "\"vps\"".into())
            .expect("reader returns a value");
        assert_eq!(missing, "null");
    }

    /// The native `set_vps_state_for_test` dispatch (web had a wasm binding for
    /// it; native silently dropped it before) seeds a verified VPS config from
    /// the same JSON the cross-app e2e sends.
    #[test]
    fn bridge_set_vps_state_seeds_verified_config() {
        use crate::observer::NullObserver;
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::new(observer);

        let json = r#"{
            "provider_id": "hetzner",
            "creds": {"api-token": "tkn"},
            "server_types": [{"id": "cax11", "vcpu": 2, "mem_gb": 4.0, "disk_gb": 40,
                              "price_monthly_cents": 451, "currency": "EUR"}],
            "selected_server_type_id": "cax11",
            "locations": [{"id": "fsn1", "name": "Falkenstein", "city": "Falkenstein", "country": "DE"}],
            "selected_location_id": "fsn1"
        }"#;
        let ret =
            machine.call_machine_method_with_result("set_vps_state_for_test".into(), json.into());
        assert_eq!(ret, None, "setter returns no value");

        let snap = machine.snapshot();
        assert_eq!(snap.vps.selected_provider_id.as_deref(), Some("hetzner"));
        assert_eq!(snap.vps.selected_server_type_id.as_deref(), Some("cax11"));
        assert_eq!(snap.vps.selected_location_id.as_deref(), Some("fsn1"));
        assert_eq!(snap.vps.server_types.len(), 1);
        assert!(snap.vps.verified);
    }

    /// `bill_of_materials()` on a fresh machine has nothing to summarize —
    /// no domain price quoted, no VPS server type selected.
    #[test]
    fn bill_of_materials_empty_on_fresh_machine() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        assert!(m.bill_of_materials().is_empty());
    }

    /// The domain line appears only when `buy_domain` is on AND the
    /// registrar quoted `Buyable` — not merely because a price was quoted
    /// (a quote with `buy_domain` off means the user chose "already own
    /// this domain" or hasn't decided, so nothing is being purchased).
    #[test]
    fn bill_of_materials_domain_line_gated_on_buy_domain() {
        use fauna_provisioning::registrar::RegistrarAvailability;

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.set_dns_state_for_test(|d| {
            d.selected_provider_id = Some("gandi".into());
            d.verified = true;
            d.current_availability = Some(RegistrarAvailability::Buyable {
                price_cents: 1099,
                currency: Some("EUR".into()),
                renewal_cents: Some(1499),
            });
            d.buy_domain = false;
        });
        assert!(
            m.bill_of_materials().is_empty(),
            "Buyable quote with buy_domain off must not appear as a charge"
        );

        m.set_dns_state_for_test(|d| d.buy_domain = true);
        let items = m.bill_of_materials();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].price_cents, 1099);
        assert_eq!(items[0].currency, "EUR");
        assert!(!items[0].recurring, "domain purchase is one-time");
        assert_eq!(items[0].label.key, "onboarding.provision.step.domain");
    }

    /// The VPS line reflects the selected server type's monthly price and
    /// currency, and is marked `recurring`. Missing/absent `currency` on
    /// the domain quote falls back to USD (mirrors `dns_status_text`'s
    /// `UnregisteredBuyable` arm — same fallback, one behavior).
    #[test]
    fn bill_of_materials_vps_line_reflects_selected_server_type() {
        use fauna_provisioning::vps::ServerTypeInfo;

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.set_vps_state_for_test(|v| {
            v.server_types = vec![
                ServerTypeInfo {
                    id: "cax11".into(),
                    vcpu: 2,
                    mem_gb: 4.0,
                    disk_gb: 40,
                    price_monthly_cents: 451,
                    currency: "EUR".into(),
                },
                ServerTypeInfo {
                    id: "cax21".into(),
                    vcpu: 4,
                    mem_gb: 8.0,
                    disk_gb: 80,
                    price_monthly_cents: 899,
                    currency: "EUR".into(),
                },
            ];
            v.selected_server_type_id = Some("cax21".into());
        });

        let items = m.bill_of_materials();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].price_cents, 899, "picks the SELECTED server type");
        assert_eq!(items[0].currency, "EUR");
        assert!(items[0].recurring, "VPS is a per-month charge");
        assert_eq!(items[0].label.key, "onboarding.provision.step.server");
    }

    /// Both lines together, in the mock's order (Domain, then VPS) — the
    /// deferred-DNS path is a separate scenario (no domain line ever, since
    /// `buy_domain` is false there by construction).
    #[test]
    fn bill_of_materials_lists_both_lines_when_buying_domain_and_vps_selected() {
        use fauna_provisioning::registrar::RegistrarAvailability;
        use fauna_provisioning::vps::ServerTypeInfo;

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.set_dns_state_for_test(|d| {
            d.selected_provider_id = Some("gandi".into());
            d.verified = true;
            d.current_availability = Some(RegistrarAvailability::Buyable {
                price_cents: 500,
                currency: None,
                renewal_cents: None,
            });
            d.buy_domain = true;
        });
        m.set_vps_state_for_test(|v| {
            v.server_types = vec![ServerTypeInfo {
                id: "cax11".into(),
                vcpu: 2,
                mem_gb: 4.0,
                disk_gb: 40,
                price_monthly_cents: 600,
                currency: "USD".into(),
            }];
            v.selected_server_type_id = Some("cax11".into());
        });

        let items = m.bill_of_materials();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label.key, "onboarding.provision.step.domain");
        assert_eq!(items[0].currency, "USD", "None currency falls back to USD");
        assert_eq!(items[1].label.key, "onboarding.provision.step.server");
    }

    /// `bom_domain_line`/`bom_vps_line` are `None` when `bill_of_materials()`
    /// has nothing to summarize — same gating, pre-derived.
    #[test]
    fn bom_lines_none_on_fresh_machine() {
        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        assert_eq!(m.bom_domain_line(), None);
        assert_eq!(m.bom_vps_line(), None);
    }

    /// No registrar-quoted renewal price → the plain `bom_line` key, with
    /// `{label}` carrying the step's own i18n key (resolved via
    /// `resolve_nested`, not `resolve` — `onboarding.md` § 6) and `{price}`
    /// the shared `format_price()` output.
    #[test]
    fn bom_domain_line_uses_bom_line_without_renewal() {
        use fauna_provisioning::registrar::RegistrarAvailability;

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.set_dns_state_for_test(|d| {
            d.selected_provider_id = Some("gandi".into());
            d.verified = true;
            d.current_availability = Some(RegistrarAvailability::Buyable {
                price_cents: 500,
                currency: None,
                renewal_cents: None,
            });
            d.buy_domain = true;
        });

        let line = m.bom_domain_line().expect("domain line present");
        assert_eq!(line.key, "onboarding.nest_provisioning.bom_line");
        assert_eq!(
            line.args.get("label").map(String::as_str),
            Some("onboarding.provision.step.domain")
        );
        assert_eq!(
            line.args.get("price").map(String::as_str),
            Some(crate::helpers::format_price(500, "USD".into())).as_deref()
        );
        assert!(!line.args.contains_key("renewal"));
    }

    /// A registrar-quoted renewal price switches to the `bom_line_domain`
    /// key, with `{renewal}` carrying the shared `format_price()` output
    /// (`BillOfMaterialsItem.renewal_price_cents`, disclosed before the
    /// charge — `onboarding.md` § 6).
    #[test]
    fn bom_domain_line_uses_bom_line_domain_with_renewal() {
        use fauna_provisioning::registrar::RegistrarAvailability;

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.set_dns_state_for_test(|d| {
            d.selected_provider_id = Some("gandi".into());
            d.verified = true;
            d.current_availability = Some(RegistrarAvailability::Buyable {
                price_cents: 1099,
                currency: Some("EUR".into()),
                renewal_cents: Some(1499),
            });
            d.buy_domain = true;
        });

        let line = m.bom_domain_line().expect("domain line present");
        assert_eq!(line.key, "onboarding.nest_provisioning.bom_line_domain");
        assert_eq!(
            line.args.get("renewal").map(String::as_str),
            Some(crate::helpers::format_price(1499, "EUR".into())).as_deref()
        );
    }

    /// The VPS line always uses `bom_line_recurring`, with `{label}`/
    /// `{price}` from the selected server type — never a renewal arg (VPS
    /// pricing has no first-year/renewal split).
    #[test]
    fn bom_vps_line_uses_bom_line_recurring() {
        use fauna_provisioning::vps::ServerTypeInfo;

        let m: Arc<OnboardingMachine> = OnboardingMachine::new(CountingObserver::new());
        m.set_vps_state_for_test(|v| {
            v.server_types = vec![ServerTypeInfo {
                id: "cax21".into(),
                vcpu: 4,
                mem_gb: 8.0,
                disk_gb: 80,
                price_monthly_cents: 899,
                currency: "EUR".into(),
            }];
            v.selected_server_type_id = Some("cax21".into());
        });

        let line = m.bom_vps_line().expect("vps line present");
        assert_eq!(line.key, "onboarding.nest_provisioning.bom_line_recurring");
        assert_eq!(
            line.args.get("label").map(String::as_str),
            Some("onboarding.provision.step.server")
        );
        assert_eq!(
            line.args.get("price").map(String::as_str),
            Some(crate::helpers::format_price(899, "EUR".into())).as_deref()
        );
        assert!(!line.args.contains_key("renewal"));
    }

    /// The native dispatch for the four sync DNS-stage methods the
    /// `go_to_vps_config_with_dns_provider` e2e helper drives: toggle
    /// same-provider, select the DNS provider, set two creds (a JSON 2-array),
    /// then `continue_from_dns` mirrors the provider + creds onto the VPS stage
    /// and advances the step — so vps_config shows it preselected. (The async
    /// `verify_dns` is driven by the per-app bridge, not this dispatcher.)
    #[test]
    fn bridge_dns_stage_methods_mirror_provider_to_vps() {
        use crate::observer::NullObserver;
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::new(observer);

        // Hetzner has the Vps capability, so the mirror fires. Its single
        // Cloud `api-token` (kinds [vps, dns]) is the only field the user
        // enters; the mirror copies it to the VPS flow.
        machine.call_machine_method("toggle_same_provider_for_vps".into(), "true".into());
        machine.call_machine_method("select_dns_provider".into(), "\"hetzner\"".into());
        machine.call_machine_method("set_dns_cred".into(), r#"["api-token","MOCK"]"#.into());

        let dns = machine.dns_config();
        assert_eq!(dns.selected_provider_id.as_deref(), Some("hetzner"));
        assert!(dns.same_provider_for_vps);
        assert_eq!(dns.creds.len(), 1);

        machine.call_machine_method("continue_from_dns".into(), String::new());

        let snap = machine.snapshot();
        assert_eq!(
            snap.step,
            OnboardingStep::VpsConfig,
            "advanced to vps_config"
        );
        assert_eq!(
            snap.vps.selected_provider_id.as_deref(),
            Some("hetzner"),
            "continue_from_dns mirrored the provider onto VPS"
        );
        assert_eq!(snap.vps.creds.len(), 1, "creds mirrored onto VPS");
        assert!(snap.vps.verified);
    }

    /// The `provisioning_snapshot` reader returns a JSON snapshot the driver
    /// can poll for orchestrator progress.
    #[test]
    fn bridge_provisioning_snapshot_reader_returns_json() {
        use crate::observer::NullObserver;
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::new(observer);
        let json = machine
            .call_machine_method_with_result("provisioning_snapshot".into(), String::new())
            .expect("reader returns a value");
        // Round-trips back into a ProvisioningSnapshot (idle: four pending steps).
        let snap: fauna_provisioning::progress::ProvisioningSnapshot =
            serde_json::from_str(&json).expect("valid ProvisioningSnapshot JSON");
        assert_eq!(snap.steps.len(), 4);
    }

    /// Seed a machine whose wizard sits on a pending invite request, with the
    /// recheck seam already answering `NotFound` — the shared arrangement of the
    /// two probe tests below and the terminal-error one.
    #[cfg(test)]
    fn machine_on_pending_invite_whose_recheck_is_not_found(
        fake: &std::sync::Arc<crate::nest_api::FakeNestApi>,
    ) -> std::sync::Arc<OnboardingMachine> {
        use crate::observer::NullObserver;

        fake.set_recheck_invite_request_response(Err(
            crate::nest_api::InviteRequestError::NotFound,
        ));
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::with_nest_api(
            observer,
            fake.clone() as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );

        // Seed an identity secret so the recheck doesn't bail on "no identity".
        machine.seed_identity("01".repeat(32));

        // Seed the wizard so recheck has a pending request to poll.
        machine.set_nest_url("https://nest.example".into());
        machine.seed_pending_invite(
            "https://nest.example".into(),
            "alice@example.test".into(),
            "req-1".into(),
            r#"{"PendingReview":{"request_id":"req-1","last_checked_ms":0}}"#.into(),
        );
        machine
    }

    /// A registered-probe fixture: the ceremony reports this secret IS
    /// registered here, under `handle`.
    #[cfg(test)]
    fn probe_says_registered(handle: &str) -> crate::nest_api::SilentChallengeOutcome {
        crate::nest_api::SilentChallengeOutcome::Success(fauna_protocol::auth::VerifyReply {
            token: "bearer".into(),
            token_id: "0".repeat(16),
            handle: handle.into(),
            domain: "example.test".into(),
            tier: "free".into(),
            expires_at: 0,
            ..Default::default()
        })
    }

    /// **Approval is detected as admission** (`onboarding.md` § The pending-invite
    /// surface): the admin approve creates the account and DELETES the request
    /// row, so the poll's `NotFound` is the approval signal. Before rendering any
    /// error the machine runs the registered-probe, and a successful verify **is**
    /// the login — `LoggedIn`/`Done`, exactly like a redeem success, with no
    /// "Approved, press Continue" interstitial.
    ///
    /// This is what makes `admin.md` § Architectural rules 5 ("approving must
    /// trigger the requester's onboarding flow to advance") true for the first
    /// time: the previous shape rendered a terminal error on approval.
    #[tokio::test]
    async fn recheck_not_found_then_registered_probe_is_the_login() {
        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        fake.set_silent_challenge_response(probe_says_registered("alice"));
        let machine = machine_on_pending_invite_whose_recheck_is_not_found(&fake);

        let step = machine.recheck_invite_status().await;

        assert_eq!(
            fake.calls(),
            vec![
                "recheck_invite_request".to_string(),
                "silent_challenge".to_string()
            ],
            "the probe must run BEFORE any error is rendered, not instead of the recheck"
        );
        assert_eq!(step, OnboardingStep::Done);
        match machine.wizard_outcome() {
            Some(WizardOutcome::LoggedIn { nest_url, handle }) => {
                // `state.nest_url`, not the provider override — the same rule
                // the redeem path follows: this URL reaches the identity store.
                assert_eq!(nest_url, "https://nest.example");
                // The wizard's typed handle, not the nest's bare localpart.
                assert_eq!(handle, "alice@example.test");
            }
            other => panic!("expected LoggedIn, got {:?}", other),
        }
    }

    /// The other arm of the same disambiguation: a verify that reports
    /// not-registered means the request is genuinely gone (cancelled elsewhere /
    /// admin-purged), so the terminal `invite.error.not_found` stands — and the
    /// per-app glue deletes the slot on it (`onboarding.md` § Persistence
    /// callouts). Supersedes the pre-2026-08-11 shape, which rendered this error
    /// for *both* outcomes and so reported approval as a dead end.
    #[tokio::test]
    async fn recheck_not_found_with_probe_refuting_registration_stays_terminal() {
        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        fake.set_silent_challenge_response(crate::nest_api::SilentChallengeOutcome::NotRegistered);
        let machine = machine_on_pending_invite_whose_recheck_is_not_found(&fake);

        let step = machine.recheck_invite_status().await;

        assert_eq!(
            fake.calls(),
            vec![
                "recheck_invite_request".to_string(),
                "silent_challenge".to_string()
            ],
            "the probe runs on every NotFound — it is what disambiguates the two causes"
        );
        assert_ne!(step, OnboardingStep::Done, "a refuted probe is not a login");
        assert!(
            machine.wizard_outcome().is_none(),
            "no outcome may be published when admission was refuted"
        );

        let snap = machine.invite_request_snapshot();
        match &snap.state {
            crate::snapshots::InviteRequestState::Error {
                cause,
                context,
                transient,
            } => {
                assert_eq!(cause, "invite.error.not_found");
                assert!(matches!(
                    context,
                    crate::snapshots::ErrorContext::Rechecking
                ));
                assert!(!*transient);
            }
            other => panic!("expected Error{{not_found, Rechecking}}, got {:?}", other),
        }
    }

    /// The cancel body must verify against the bytes the NEST reconstructs
    /// (`cancel_invite_request_core`: `actor_id ‖ b"cancel" ‖ timestamp_be`), not
    /// merely be well-formed. Rebuild that message here and check the signature
    /// against the actor id the body itself carries — a builder that signed the
    /// wrong bytes would still produce a perfectly-shaped struct, and the
    /// resubmit would fail only against a live nest.
    #[test]
    fn the_cancel_body_is_signed_over_the_bytes_the_nest_verifies() {
        // verify-ok(test): this module signs with a locally generated key and checks
        // its own signature back — no wire-supplied key reaches it, so the permissive
        // trait is harmless here. Production verification goes through
        // `fauna_core::identity::verify_detached`; the walk guard
        // `fauna-core/tests/one_ed25519_verification_shape.rs` reads this marker.
        use ed25519_dalek::{Signature, Verifier};

        let secret_hex = "01".repeat(32);
        let body = build_invite_request_cancel(&secret_hex).expect("valid secret builds a body");

        let actor_id_bytes: [u8; 32] = hex::decode(&body.actor_id).unwrap().try_into().unwrap();
        let expected =
            fauna_protocol::invite::invite_cancel_signed_message(&actor_id_bytes, body.timestamp);

        let vk = ed25519_dalek::VerifyingKey::from_bytes(&actor_id_bytes).unwrap();
        let sig_bytes: [u8; 64] = hex::decode(&body.signature).unwrap().try_into().unwrap();
        assert!(
            vk.verify(&expected, &Signature::from_bytes(&sig_bytes))
                .is_ok(),
            "the cancel signature must cover the tagged invite_cancel_signed_message"
        );

        // The domain tag is the replay guard: the same key signing the bare
        // untagged body must NOT produce a signature that verifies here.
        let mut without_domain_separator = Vec::new();
        without_domain_separator.extend_from_slice(&actor_id_bytes);
        without_domain_separator.extend_from_slice(&body.timestamp.to_be_bytes());
        assert!(
            vk.verify(
                &without_domain_separator,
                &Signature::from_bytes(&sig_bytes)
            )
            .is_err(),
            "dropping the domain separator must invalidate the signature"
        );
    }

    /// **A denied requester can re-apply.** The nest refuses a second request
    /// while any row exists for the actor, so the wizard withdraws the denied row
    /// first and submits in the same gesture (`onboarding.md` § The
    /// pending-invite surface — "re-submitting runs the cancel-then-submit
    /// sequence"). Pure client sequencing; no wire change. Before this, a denied
    /// user was stuck forever with no affordance at all.
    #[tokio::test]
    async fn resubmitting_from_denied_cancels_the_denied_row_first() {
        use crate::observer::NullObserver;
        use crate::snapshots::{InviteRequestSnapshot, InviteRequestState, OobCodeState};

        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::with_nest_api(
            observer,
            fake.clone() as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );
        machine.seed_identity("01".repeat(32));
        machine.set_nest_url("https://nest.example".into());
        machine.set_current_handle("alice@example.test".into());
        machine.set_invite_request_snapshot_for_test(InviteRequestSnapshot {
            state: InviteRequestState::Denied {
                reason: "not now".into(),
                request_id: "req-1".into(),
            },
            message: LocalizedText::default(),
            continue_enabled: false,
            recheck_visible: false,
            out_of_band_code_state: OobCodeState::Idle,
            oob_message: LocalizedText::default(),
            age_notice: None,
        });

        let _ = machine.wizard_submit_invite_request().await;

        assert_eq!(
            fake.calls(),
            vec![
                "cancel_invite_request".to_string(),
                "submit_invite_request".to_string()
            ],
            "cancel must precede submit — the nest refuses a submit while the denied row exists"
        );
    }

    /// The cancel is NOT run from a fresh `Idle` page: there is no row to
    /// withdraw, and firing it anyway would spend a pre-identity RPC on every
    /// first-time applicant.
    #[tokio::test]
    async fn a_first_submit_does_not_cancel_anything() {
        use crate::observer::NullObserver;

        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::with_nest_api(
            observer,
            fake.clone() as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );
        machine.seed_identity("01".repeat(32));
        machine.set_nest_url("https://nest.example".into());
        machine.set_current_handle("alice@example.test".into());

        let _ = machine.wizard_submit_invite_request().await;

        assert_eq!(
            fake.calls(),
            vec!["submit_invite_request".to_string()],
            "a first-time submit has no row to withdraw"
        );
    }

    /// A probe that cannot reach the nest must NOT be read as "not registered" —
    /// that would render a terminal, slot-deleting error for a user whose request
    /// is very much alive, on nothing worse than a dropped connection. An
    /// unreachable probe leaves the poll where it was: still `PendingReview`,
    /// so the next tick simply asks again.
    #[tokio::test]
    async fn recheck_not_found_with_unreachable_probe_keeps_polling() {
        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        fake.set_silent_challenge_response(crate::nest_api::SilentChallengeOutcome::Transient {
            error: "connect refused".into(),
        });
        let machine = machine_on_pending_invite_whose_recheck_is_not_found(&fake);

        let step = machine.recheck_invite_status().await;

        assert_ne!(step, OnboardingStep::Done);
        assert!(machine.wizard_outcome().is_none());
        let snap = machine.invite_request_snapshot();
        assert!(
            matches!(
                &snap.state,
                crate::snapshots::InviteRequestState::PendingReview { request_id, .. }
                    if request_id == "req-1"
            ),
            "an unreachable probe must leave the request pending, got {:?}",
            snap.state
        );
        assert!(
            snap.recheck_visible,
            "the page must keep its recheck affordance while still pending"
        );
    }

    /// Real bug found while writing the family-safety windows e2e:
    /// `verify_oob_invite_code` set `out_of_band_code_state` to `Valid` on success
    /// but never touched `continue_enabled`, so `invite-request-continue-button`
    /// stayed disabled on every app after a successful OOB code check —
    /// `redeem_invite`'s own routing condition (`state == Approved || oob ==
    /// Valid`) never became reachable via the UI. The prior tier_2 coverage
    /// (`test_invite_request_states.py::test_oob_valid_enables_continue`) never
    /// caught this: it hand-authors a snapshot with both fields already
    /// consistent, never exercising the real transition.
    /// The age claim set on the machine rides BOTH admission bodies verbatim —
    /// the shared carry every mobile store-signal arm plugs into
    /// (`family-safety.md` § The account age band): the wire type is built
    /// here, never in Kotlin/Swift. A factory reset drops it with the rest of
    /// the run's progress.
    #[tokio::test]
    async fn an_age_claim_set_on_the_machine_rides_both_admission_bodies() {
        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        let observer = std::sync::Arc::new(crate::observer::NullObserver)
            as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::with_nest_api(
            observer,
            fake.clone() as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );
        machine.seed_identity("01".repeat(32));
        machine.navigate_to_invite_request_for_known_nest(
            "https://nest.example".into(),
            "alice@nest.example".into(),
        );
        assert_eq!(machine.age_claim(), None, "no claim until an app sets one");

        // The nest says it can check android, over the nonce the glue binds.
        fake.set_age_nonce_response(Ok(crate::nest_api::AgeNonce {
            nonce_hex: "ab".repeat(32),
            expires_in_secs: 300,
            attestation_platforms: vec!["android".into()],
        }));
        let minted = machine.request_age_nonce().await.expect("nonce minted");
        assert_eq!(minted.attestation_platforms, vec!["android".to_string()]);

        let claim = crate::state::AgeClaimPlain {
            band: "13-15".into(),
            attestation: Some(crate::state::AgeAttestationPlain {
                platform: "android".into(),
                nonce_hex: "ab".repeat(32),
                key_id_hex: String::new(),
                attestation_object: b"jwe.token".to_vec(),
            }),
        };
        machine.set_age_claim(Some(claim.clone()));
        assert_eq!(machine.age_claim(), Some(claim.clone()));

        // The notice is derived from the claim on every snapshot read — an
        // attested android claim names Google Play + Google; clearing the
        // claim clears the notice (the shell renders nothing).
        let notice = machine
            .invite_request_snapshot()
            .age_notice
            .expect("an attested claim yields the notice");
        assert_eq!(notice.key, "family.age_band.notice_attested");
        assert_eq!(notice.args["band"], "family.age_band.teen_13_15");
        assert_eq!(notice.args["store"], "family.age_band.store_android");
        machine.set_age_claim(Some(crate::state::AgeClaimPlain {
            band: "13-15".into(),
            attestation: None,
        }));
        assert_eq!(
            machine.invite_request_snapshot().age_notice.map(|t| t.key),
            Some("family.age_band.notice_declared".into()),
            "a declared-only claim still shows what will be shared"
        );
        machine.set_age_claim(None);
        assert_eq!(machine.invite_request_snapshot().age_notice, None);
        machine.set_age_claim(Some(claim.clone()));

        // The invite-request path.
        machine.wizard_submit_invite_request().await;
        let bodies = fake.submit_invite_request_bodies();
        assert_eq!(bodies.len(), 1, "calls: {:?}", fake.calls());
        let carried = bodies[0]
            .age_claim
            .as_ref()
            .expect("the submit body carries the claim");
        assert_eq!(carried.band, "13-15");
        let att = carried.attestation.as_ref().expect("attested");
        assert_eq!(att.platform, "android");
        assert_eq!(att.nonce, "ab".repeat(32));
        assert_eq!(att.key_id, "");
        assert_eq!(att.attestation_object.as_ref(), b"jwe.token");

        // The register (code-redeem) path.
        machine.verify_oob_invite_code("code-123".into()).await;
        machine.redeem_invite().await;
        let bodies = fake.register_bodies();
        assert_eq!(bodies.len(), 1, "calls: {:?}", fake.calls());
        assert_eq!(
            bodies[0]
                .age_claim
                .as_ref()
                .map(crate::state::AgeClaimPlain::from),
            Some(claim)
        );

        machine.reset();
        assert_eq!(machine.age_claim(), None, "reset clears the claim");
    }

    /// The send guard (`family-safety.md` § The account age band → *An
    /// attestation the nest cannot check*): an attestation rides the wire only
    /// when the last mint was from this nest, minted exactly its nonce, and
    /// listed its platform. Every other case sends the claim declared-only, and
    /// the notice follows what is sent — never "verified" for a claim the nest
    /// would record as `none`. `age_claim()` keeps what the glue set.
    #[tokio::test]
    async fn an_attestation_the_nest_did_not_list_rides_declared_only() {
        fn attested(platform: &str, nonce: &str) -> crate::state::AgeClaimPlain {
            crate::state::AgeClaimPlain {
                band: "13-15".into(),
                attestation: Some(crate::state::AgeAttestationPlain {
                    platform: platform.into(),
                    nonce_hex: nonce.into(),
                    key_id_hex: String::new(),
                    attestation_object: b"token".to_vec(),
                }),
            }
        }
        fn mint(fake: &crate::nest_api::FakeNestApi, nonce: &str, platforms: &[&str]) {
            fake.set_age_nonce_response(Ok(crate::nest_api::AgeNonce {
                nonce_hex: nonce.into(),
                expires_in_secs: 300,
                attestation_platforms: platforms.iter().map(|p| p.to_string()).collect(),
            }));
        }
        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        let observer = std::sync::Arc::new(crate::observer::NullObserver)
            as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::with_nest_api(
            observer,
            fake.clone() as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );
        machine.seed_identity("03".repeat(32));
        machine.navigate_to_invite_request_for_known_nest(
            "https://nest.example".into(),
            "carol@nest.example".into(),
        );
        let n1 = "ab".repeat(32);
        let n2 = "cd".repeat(32);
        let sent_attested = |m: &OnboardingMachine| {
            m.age_claim_to_send()
                .expect("a claim is set")
                .attestation
                .is_some()
        };
        let notice_key = |m: &OnboardingMachine| {
            m.invite_request_snapshot()
                .age_notice
                .map(|t| t.key)
                .expect("a named band yields a notice")
        };

        // No nonce minted at all.
        machine.set_age_claim(Some(attested("android", &n1)));
        assert!(!sent_attested(&machine));
        assert_eq!(notice_key(&machine), "family.age_band.notice_declared");
        assert!(
            machine.age_claim().unwrap().attestation.is_some(),
            "age_claim() reports what the glue set, attestation included"
        );

        // An unarmed nest (the fake's default) or one that lists nothing.
        machine.request_age_nonce().await.expect("mint");
        assert!(!sent_attested(&machine));
        mint(&fake, &n1, &[]);
        machine.request_age_nonce().await.expect("mint");
        assert!(!sent_attested(&machine));

        // The nest lists a different platform.
        mint(&fake, &n1, &["ios"]);
        machine.request_age_nonce().await.expect("mint");
        assert!(!sent_attested(&machine));
        assert_eq!(notice_key(&machine), "family.age_band.notice_declared");

        // Listed, but the attestation binds a different nonce than the last mint.
        mint(&fake, &n2, &["android"]);
        machine.request_age_nonce().await.expect("mint");
        assert!(!sent_attested(&machine));

        // Listed AND the nonce matches → the attestation rides, notice verified.
        machine.set_age_claim(Some(attested("android", &n2)));
        assert!(sent_attested(&machine));
        assert_eq!(notice_key(&machine), "family.age_band.notice_attested");

        // The admission now goes to another nest than the one that minted.
        machine.navigate_to_invite_request_for_known_nest(
            "https://other.example".into(),
            "carol@other.example".into(),
        );
        assert!(!sent_attested(&machine));
        assert_eq!(notice_key(&machine), "family.age_band.notice_declared");

        // And the submit body carries the declared-only claim, band intact.
        machine.wizard_submit_invite_request().await;
        let bodies = fake.submit_invite_request_bodies();
        let carried = bodies
            .last()
            .and_then(|b| b.age_claim.as_ref())
            .expect("the submit body carries the claim");
        assert_eq!(carried.band, "13-15");
        assert!(carried.attestation.is_none(), "stripped, never sent");
    }

    /// The attested payload is minted from the machine's own identity, so the
    /// app never assembles it: the bytes are exactly the protocol builder's —
    /// the one definition the nest's verifier recomputes.
    #[tokio::test]
    async fn the_age_claim_message_is_the_protocol_builders_bytes_for_this_identity() {
        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        let observer = std::sync::Arc::new(crate::observer::NullObserver)
            as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::with_nest_api(
            observer,
            fake.clone() as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );
        assert!(
            machine
                .age_claim_message("ab".repeat(32), "U13".into(), "social.fauna.fauna".into())
                .is_err(),
            "no identity yet → nothing to bind"
        );

        let secret_hex = "02".repeat(32);
        machine.seed_identity(secret_hex.clone());
        machine.navigate_to_invite_request_for_known_nest(
            "https://nest.example".into(),
            "bob@nest.example".into(),
        );
        let nonce = machine.request_age_nonce().await.expect("nonce minted");
        assert_eq!(nonce.nonce_hex, "ab".repeat(32));
        assert_eq!(nonce.expires_in_secs, 300);
        assert!(fake.calls().contains(&"age_nonce".to_string()));

        let message = machine
            .age_claim_message(nonce.nonce_hex, "U13".into(), "social.fauna.fauna".into())
            .expect("message minted");
        let actor = parse_signing_key(&secret_hex)
            .expect("test secret parses")
            .verifying_key()
            .to_bytes();
        assert_eq!(
            message,
            fauna_protocol::age::age_claim_signed_message(
                &[0xab; 32],
                "U13",
                "social.fauna.fauna",
                &actor
            )
        );
        {
            use sha2::Digest as _;
            let digest = machine
                .age_claim_digest("ab".repeat(32), "U13".into(), "social.fauna.fauna".into())
                .expect("digest minted");
            assert_eq!(
                digest,
                sha2::Sha256::digest(&message).to_vec(),
                "the digest both platforms bind is SHA-256 of the message"
            );
        }
        assert!(
            machine
                .age_claim_message("not-hex".into(), "U13".into(), "social.fauna.fauna".into())
                .is_err(),
            "a malformed nonce is refused"
        );
    }

    #[tokio::test]
    async fn verify_oob_invite_code_valid_enables_continue() {
        use crate::nest_api::InviteCodeVerification;
        use crate::observer::NullObserver;

        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        fake.set_verify_invite_code_response(Ok(InviteCodeVerification {
            invite_id: "inv-1".into(),
            supervised_by: Some("guardian1".into()),
        }));
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::with_nest_api(
            observer,
            fake as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );
        machine.set_nest_url("https://nest.example".into());

        machine.verify_oob_invite_code("code-123".into()).await;

        let snap = machine.invite_request_snapshot();
        assert!(
            matches!(
                snap.out_of_band_code_state,
                crate::snapshots::OobCodeState::Valid { .. }
            ),
            "expected OobCodeState::Valid, got {:?}",
            snap.out_of_band_code_state
        );
        assert!(
            snap.continue_enabled,
            "continue_enabled must be true once the OOB code verifies Valid — \
             redeem_invite() routes on exactly this condition"
        );
    }

    /// The reverse transition: a previously-valid OOB code re-checked to
    /// Invalid must revert `continue_enabled` to false (absent an independent
    /// Approved/PendingReview top-level state) — a stale `true` would let the
    /// user redeem a code that no longer verifies.
    #[tokio::test]
    async fn verify_oob_invite_code_invalid_after_valid_disables_continue() {
        use crate::nest_api::InviteCodeVerification;
        use crate::observer::NullObserver;

        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        fake.set_verify_invite_code_response(Ok(InviteCodeVerification {
            invite_id: "inv-1".into(),
            supervised_by: None,
        }));
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::with_nest_api(
            observer,
            fake.clone() as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );
        machine.set_nest_url("https://nest.example".into());
        machine.verify_oob_invite_code("code-123".into()).await;
        assert!(machine.invite_request_snapshot().continue_enabled);

        fake.set_verify_invite_code_response(Err(crate::nest_api::InviteCodeError::Invalid {
            reason: "expired".into(),
        }));
        machine.verify_oob_invite_code("code-123".into()).await;

        let snap = machine.invite_request_snapshot();
        assert!(
            matches!(
                snap.out_of_band_code_state,
                crate::snapshots::OobCodeState::Invalid { .. }
            ),
            "expected OobCodeState::Invalid, got {:?}",
            snap.out_of_band_code_state
        );
        assert!(
            !snap.continue_enabled,
            "continue_enabled must revert to false once the OOB code no longer verifies"
        );
    }

    /// `redeem_invite` must register the handle's BARE LOCAL PART, signed over
    /// `(local_part, domain)`.
    ///
    /// Two nest-side rules meet here, and the redeem has to satisfy both at once
    /// (`account_core::register_core`):
    ///
    /// 1. `validate_handle` rejects `@` ("lowercase alphanumeric or hyphens"),
    ///    and it runs BEFORE the signature check — so the whole wizard handle
    ///    `alice@nest.example` is refused outright.
    /// 2. The nest recomputes `domain` from its own configured handle domain and
    ///    verifies over `actor_id || handle || <candidate domain> || ts_be`,
    ///    trying only its handle domain + active mail domains — never the empty
    ///    string. So a bare handle signed over `""` fails the signature check.
    ///
    /// Sending the whole handle trips (1); sending a bare handle without
    /// carrying its `@domain` into the signature trips (2). The wizard's own
    /// handle format is `user@domain` (`test_onboarding_localhost.py` types
    /// `test@localhost:<port>`), so before this was pinned, redeeming an OOB
    /// invite code through the wizard could not succeed on any nest — on any of
    /// the six apps, since this is the shared machine.
    ///
    /// The claim path has always split correctly (`handle_local_part` +
    /// `parse_handle_domain`, pinned by `claim_admin_args`); this is the same
    /// assertion for the register path, which had no body capture at all.
    #[tokio::test]
    async fn redeem_invite_registers_the_bare_local_part_signed_over_the_handle_domain() {
        use crate::nest_api::InviteCodeVerification;
        use crate::observer::NullObserver;

        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        fake.set_verify_invite_code_response(Ok(InviteCodeVerification {
            invite_id: "inv-1".into(),
            supervised_by: None,
        }));
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::with_nest_api(
            observer,
            fake.clone() as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );

        let secret_hex = "01".repeat(32);
        machine.seed_identity(secret_hex.clone());
        machine.navigate_to_invite_request_for_known_nest(
            "https://nest.example".into(),
            "alice@nest.example".into(),
        );
        machine.verify_oob_invite_code("code-123".into()).await;
        machine.redeem_invite().await;

        let bodies = fake.register_bodies();
        assert_eq!(
            bodies.len(),
            1,
            "redeem_invite must call register exactly once; calls: {:?}",
            fake.calls()
        );
        let body = &bodies[0];

        assert_eq!(
            body.handle, "alice",
            "the nest stores the BARE local part: `validate_handle` rejects '@', so sending \
             the whole wizard handle is refused as InvalidRequest before the signature is \
             even looked at, and the redeem can never succeed"
        );

        let signing_key = parse_signing_key(&secret_hex).expect("test secret parses");
        let actor_id_bytes = signing_key.verifying_key().to_bytes();
        assert_eq!(
            body.actor_id,
            hex::encode(actor_id_bytes),
            "the body must carry the identity's own actor_id"
        );

        let expected = fauna_protocol::account::register_signed_message(
            &actor_id_bytes,
            "alice",
            "nest.example",
            body.timestamp,
        );
        let sig_bytes = hex::decode(&body.signature).expect("signature is hex");
        let signature =
            ed25519_dalek::Signature::from_slice(&sig_bytes).expect("64-byte signature");
        signing_key
            .verifying_key()
            .verify_strict(&expected, &signature)
            .expect(
                "the register signature must cover (actor_id || bare_handle || domain || ts_be) \
                 with the handle's own `@domain` as the domain — the nest recomputes that domain \
                 from its configured handle_domain and tries no other candidate, so signing over \
                 the empty domain is rejected as signature_failed",
            );
    }

    /// The invite-request twin of the register pin above. The submit door
    /// runs `validate_handle` (no '@') before anything else and verifies the
    /// signature over the bare lowercased handle with no domain
    /// (`invite_core::submit_invite_request_core`), so the wizard's whole
    /// `alice@nest.example` must be split to its local part. Sending it whole
    /// was refused as `invalid_request` and rendered "Something went wrong…
    /// Try again." for every typed handle — which also hid the
    /// already-registered refusal behind it (the nest validates the handle
    /// first).
    #[tokio::test]
    async fn invite_submit_sends_the_bare_local_part_signed_over_it() {
        use crate::observer::NullObserver;

        let fake = std::sync::Arc::new(crate::nest_api::FakeNestApi::new());
        let observer = std::sync::Arc::new(NullObserver) as std::sync::Arc<dyn OnboardingObserver>;
        let machine = OnboardingMachine::with_nest_api(
            observer,
            fake.clone() as std::sync::Arc<dyn crate::nest_api::NestApi>,
        );
        let secret_hex = "01".repeat(32);
        machine.seed_identity(secret_hex.clone());
        machine.navigate_to_invite_request_for_known_nest(
            "https://nest.example".into(),
            "alice@nest.example".into(),
        );
        machine.wizard_submit_invite_request().await;

        let bodies = fake.submit_invite_request_bodies();
        assert_eq!(bodies.len(), 1, "calls: {:?}", fake.calls());
        let body = &bodies[0];
        assert_eq!(
            body.handle, "alice",
            "the nest stores and validates the bare local part"
        );

        let signing_key = parse_signing_key(&secret_hex).expect("test secret parses");
        let actor_id_bytes = signing_key.verifying_key().to_bytes();
        let expected = fauna_protocol::invite::invite_submit_signed_message(
            &actor_id_bytes,
            "alice",
            &body.message,
            body.timestamp,
        );
        let sig_bytes = hex::decode(&body.signature).expect("signature is hex");
        let signature =
            ed25519_dalek::Signature::from_slice(&sig_bytes).expect("64-byte signature");
        signing_key
            .verifying_key()
            .verify_strict(&expected, &signature)
            .expect("the submit signature must cover the bare local part the body carries");
    }
}
