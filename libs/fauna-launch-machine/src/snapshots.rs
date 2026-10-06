//! Observable snapshot the LaunchMachine exposes to its observer.
//!
//! Clients read these via the machine's `snapshot()` getter after every
//! `on_changed()` notification.

use serde::{Deserialize, Serialize};

/// Top-level observable shape. Cheap to clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LaunchSnapshot {
    pub phase: LaunchPhase,
    pub token: TokenStatus,
    /// Human-readable reason for the most recent failure. Clients display
    /// it in error surfaces; mapping to localized i18n keys is left to
    /// the client.
    pub last_error: Option<String>,
    /// Set when the launch flow was refused with `fauna.auth.superseded`: the
    /// identity was succeeded and the account belongs to this actor id (64-hex)
    /// now (`docs/goal/behavior/identity-succession.md` § Propagation → *Own
    /// device fleet*). `None` in every other case.
    ///
    /// **Claimed, not proven.** This is whatever the refusal named; a client
    /// verifies it against the registration chain
    /// (`fauna_client_recovery::resolve_successor`) before presenting it as
    /// fact, because the nest is enforcer and distributor, never authorizer.
    ///
    /// An **additive side channel** rather than a `LaunchPhase` variant, on
    /// purpose — `State::Superseded`'s docs carry the full reasoning. The phase
    /// is `Offline { transient: false }`, so an app that never reads this field
    /// still stops retrying and still shows `last_error`; reading it is what
    /// upgrades a dead end into "import the new identity".
    pub superseded_successor: Option<String>,
    /// `true` when the current `LaunchPhase::IdentityChanged` is rotation-chain
    /// **fork evidence** (`box-recovery.md` § Client acceptance): the box's
    /// served rotation history contradicts the one this client accepted, so
    /// there is **no re-trust on that surface** — the machine refuses
    /// `trust_nest_identity()`, and an app reading this field hides/disables
    /// the trust affordance and names the fork. `false` in every other case,
    /// including the ordinary changed/withdrawn warnings (whose explicit
    /// re-trust is unchanged).
    ///
    /// An **additive field** rather than a `LaunchPhase` field for the same
    /// exhaustive-switch reason as `superseded_successor` (`State::Superseded`'s
    /// docs). An app that never reads it still blocks (the machine-side refusal
    /// is the security boundary); reading it is what upgrades the surface from
    /// "a trust button that refuses" to the honest fork warning.
    #[serde(default)]
    pub identity_fork: bool,
    /// The account identity the nest confirmed on the most recent successful
    /// silent challenge — `None` until one resolves.
    ///
    /// This is the **live identity channel** every native app shares
    /// (`docs/goal/ui/conversations.md` § State & data shape → *Self-address:
    /// live, never baked*: "a client calls the setter from the one place
    /// identity state lands"). The machine already learned all three fields from
    /// the `VerifyReply` and wrote them to the long-term store via
    /// `LaunchPersistence::save_authenticated` — but a store write is not an
    /// event, so an app observing only `on_changed()` had no way to notice the
    /// identity resolving or changing. Surfacing it here is what lets an app
    /// push `<handle>@<domain>` into `ConversationsSession::set_self_address`
    /// (and refresh its own caches) the moment it lands, instead of assembling
    /// an address from a cache written on some earlier run.
    ///
    /// **`None` is not an error state** — it is "not resolved yet", and per the
    /// same § identity resolution must never delay conversation delivery: the
    /// app builds its session immediately, renders whatever it cached for
    /// "Welcome back", and refuses a send locally until a real address arrives.
    /// A token refresh does not clear it: a refresh ignores its verify reply's
    /// metadata (the launch owns the cached identity), so the last confirmed one
    /// stands.
    pub identity: Option<LaunchIdentity>,
    /// Set when the account index at `fauna/index` is present but this build
    /// cannot use it (`version-compatibility.md` § 5 item 9). `None` in every
    /// other case, including a fresh install with no index at all.
    ///
    /// An **additive side channel** rather than a `LaunchPhase` variant, on the
    /// `superseded_successor` pattern above and for the same reason: the phase
    /// is `Offline { transient: false }`, so an app that never reads this field
    /// still stops retrying and still shows `last_error`, while reading it is
    /// what upgrades a dead end into the right offer — "update the app" for the
    /// version case, and for the malformed case the only case that may offer
    /// the documented floor.
    ///
    /// Its presence is also what keeps the user **out of fresh onboarding**:
    /// on either verdict the registry answers no session account, so without
    /// this the launch machine reads the install as identity-less and routes to
    /// `WizardAt { IdentityChoice }` — offering to make a new identity to
    /// someone whose accounts are sitting intact behind an unparsed blob.
    #[serde(default)]
    pub account_index_refusal: Option<crate::AccountIndexRefusal>,
    /// `true` when a nest this app had **signed in to before** — the stored
    /// identity + `nest_url` the silent-challenge row runs on — answered the
    /// opaque `fauna.auth.not_registered`, and the nest is claimed
    /// (`docs/goal/behavior/onboarding.md` § App-launch routing → *the
    /// previously-signed-in row*; `login.md` § Silent Challenge). The nest no
    /// longer signs this identity in: suspended, or removed — the app cannot
    /// tell, by design (no suspended-vs-unregistered oracle on the wire), and
    /// the copy asserts neither. Set by the launch arm and by the mid-session
    /// refresh arm alike (`security.md` § Post-auth surfacing), `false` in
    /// every other case, including an unregistered identity on an
    /// **unclaimed** nest (that is the claim-code row) and the fresh-install
    /// rows that never reach verify.
    ///
    /// An **additive side channel** on the `account_index_refusal` pattern
    /// above: the phase is `Offline { transient: false }` with the localized
    /// `onboarding.launch.sign_in_refused` in `last_error`, so an app that
    /// never reads this field still stops routing into the invite wizard and
    /// shows the honest sentence; reading it is what upgrades that dead end
    /// into the `launch_sign_in_refused` surface — the same copy, plus
    /// **Retry** (the one terminal offline whose retry the machine honours,
    /// because the admin's restore is a button on *their* app and a retry is
    /// the user's way back in) and "Use a different nest".
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = false))]
    pub sign_in_refused: bool,
    /// The unlock time (Unix seconds) when the machine is parked on
    /// `fauna.auth.account_locked` (`devices.md` § The locked state); `None` in
    /// every other case.
    ///
    /// An **additive side channel** on the `superseded_successor` pattern: the
    /// phase is `Offline { transient: false }` with a time-free localized
    /// `onboarding.launch.account_locked` in `last_error`, so an app that never
    /// reads this field still stops retrying and shows an honest sentence;
    /// reading it is what upgrades the dead end into the locked surface (the
    /// unlock time through the shared `format_unix_local`).
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub locked_until_secs: Option<u64>,
}

/// The account identity a successful silent challenge confirmed — the
/// `handle`/`domain`/`tier` triple `VerifyReply` carries.
///
/// `domain` is the **handle's** domain as the nest reports it, never the nest
/// URL's host: a nest serves handles on domains that need not be its hostname,
/// and composing an address from the dialed host is the exact bug
/// `conversations.md` § *Self-address: live, never baked* forbids (it
/// mis-routes same-nest vs. cross-nest, not just the `From:` label).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LaunchIdentity {
    /// The actor's handle, bare (no `@domain`). Empty when the account has none
    /// yet — an app pairs empty with `domain` into no address at all, never into
    /// the `"@nest.example"` shape the § names as forbidden.
    pub handle: String,
    /// The handle's domain.
    pub domain: String,
    /// The actor's tier name.
    pub tier: String,
}

impl LaunchSnapshot {
    /// Initial snapshot — Boot phase, no token, no error. Same shape the
    /// machine starts with before `start()` runs.
    pub fn initial() -> Self {
        Self {
            phase: LaunchPhase::Boot,
            token: TokenStatus::None,
            last_error: None,
            superseded_successor: None,
            identity_fork: false,
            identity: None,
            account_index_refusal: None,
            sign_in_refused: false,
            locked_until_secs: None,
        }
    }
}

/// Coarse-grained phase the launch flow is in. Drives client UI:
/// Boot/Hydrating → splash; SilentChallenge/Refreshing → spinner with
/// optional cached metadata; Online → main app; Offline → retry surface;
/// WizardAt → mount the OnboardingMachine at the given entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum LaunchPhase {
    /// Initial state before the machine has done anything.
    Boot,
    /// Reading the long-term store to determine which branch to take.
    Hydrating,
    /// Existing-account fast path: silent challenge in flight (`/auth/challenge`
    /// then `/auth/verify`). `attempt` increments on retry.
    SilentChallenge { attempt: u32 },
    /// Authenticated; a token refresh is in flight.
    Refreshing { reason: RefreshReason },
    /// Authenticated; token is valid; the main app should render.
    Online,
    /// Network or server failure prevented authentication. `transient: true`
    /// shows a retry indicator on the launch screen; `transient: false` is
    /// terminal (e.g. account locked) and the client surfaces it as such.
    Offline { transient: bool },
    /// Long-term store says: drop into the onboarding wizard at this entry.
    /// The client mounts an `OnboardingMachine` and seeds it from its own
    /// persistence (the LaunchMachine doesn't carry wizard state).
    WizardAt { entry: LaunchWizardEntry },
    /// The nest's **pinned deployment identity** changed — or a pinned nest
    /// could no longer prove any identity (`seen_hex: None`, the
    /// withdrawn/downgrade case). The SSH `known_hosts` model (security.md
    /// § Transport trust): auto-entry is BLOCKED; the client
    /// renders the `launch_identity_changed` warning surface
    /// (`nest-identity-changed-warning`) with two explicit ways out —
    /// "trust this nest" → [`LaunchMachine::trust_nest_identity`] (forget the
    /// pin, re-TOFU, re-run the silent challenge) and "use a different nest"
    /// → the wizard fallthrough. Never auto-repinned, never a retry loop.
    /// Fingerprints are hex `nest_actor_id`s for the warning's detail line;
    /// the nest's address comes from the client's own persistence.
    IdentityChanged {
        pinned_hex: String,
        seen_hex: Option<String>,
    },
}

/// Entry points the launch flow's four-case branch can hand the wizard.
/// Maps to the `OnboardingMachine`'s step enum on the client side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum LaunchWizardEntry {
    /// Long-term store has no identity. Wizard shows IdentityChoice.
    IdentityChoice,
    /// Identity present, no nest_url, no pending invite. Wizard shows HandleEntry.
    HandleEntry,
    /// Identity present, no nest_url, pending invite present. Wizard shows InviteRequest.
    InviteRequest,
    /// Identity present + an awaiting-manual-dns slot: the user provisioned a
    /// nest, chose "Set up later" for DNS, and quit. Checked **before** the
    /// silent-challenge row — while DNS is pending the nest is unreachable by
    /// definition, so challenging a saved `nest_url` would only fall through to
    /// `launch_retry`.
    ///
    /// The client seeds `seed_identity(secret)` + `seed_awaiting_manual_dns(
    /// nest_url, handle, dns_records, claim_code)` from the slot and renders the
    /// "Almost ready" surface, which polls `recheck_manual_dns()` until the nest
    /// resolves and the claim lands. This is *not* an `OnboardingStep` — the
    /// surface is keyed on `wizard_outcome() == AwaitingManualDns`, so the
    /// same-session exit and this relaunch-hydration path render identically.
    /// See `docs/goal/behavior/onboarding.md` § App-launch routing +
    /// § "Almost ready" surface.
    AwaitingManualDns,
    /// Saved identity + saved nest URL where /verify returned 404 AND
    /// `setup-status.claimed == false`. The nest is up but unclaimed,
    /// so the user must claim it themselves before any registration is
    /// possible. Wizard shows ClaimCode. See
    /// `docs/goal/behavior/onboarding.md` § App-launch routing — silent-challenge
    /// fallback table (unclaimed-nest row).
    ClaimCode,
    /// Identity present + a pending-factory-reset slot: the user factory-reset
    /// their nest and the client died (or was quit) before the re-claim
    /// completed. Checked **before** every other row — the box was wiped to
    /// fresh/unclaimed, so a silent challenge against the saved `nest_url` would
    /// only fall through to `launch_retry`, and the claim the user owes is the
    /// one the slot pins.
    ///
    /// The distinction from [`Self::ClaimCode`] is the **pre-filled code**: the
    /// client minted and pinned it before dispatching the reset (gap CR-1,
    /// `docs/goal/architecture/nest/common.md` § Client-state recoverability), so
    /// it can seed `navigate_to_claim_code_for_known_nest_with_code(nest_url,
    /// handle, claim_code)` from the slot rather than asking the user for a code
    /// that only ever existed in a reply their client never rendered.
    PendingFactoryReset,
}

/// Where the bearer token lives in the lifecycle. Surfaced so HTTP layers
/// can decide whether to wait for a refresh or proceed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TokenStatus {
    /// No token present (initial state, or after sign-out).
    None,
    /// Token is valid. `expires_at_secs` is unix seconds **on this device's
    /// clock**: the nest's `expires_in` anchored to the launch clock at receipt
    /// (`fauna_protocol::auth::deadline_on_own_clock`), so every deadline the
    /// machine and its apps derive from it is compared against the clock it was
    /// made on. Only when the clock could not be read at receipt is it the
    /// nest's raw `expires_at`.
    Valid { expires_at_secs: u64 },
    /// Token has passed its TTL but no refresh is in flight yet.
    Expired,
    /// A refresh is in flight; HTTP layers should wait.
    Refreshing,
}

/// Why the LaunchMachine kicked off a token refresh. Observer-visible so
/// telemetry can distinguish proactive refresh from reactive 401 handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RefreshReason {
    /// Pre-expiry refresh fired by the TTL scheduler.
    ScheduledTtl,
    /// HTTP layer reported a 401; LaunchMachine is re-issuing the token.
    Got401,
    /// Caller asked for a refresh explicitly.
    Manual,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_phase_boot_round_trip() {
        let p = LaunchPhase::Boot;
        let s = serde_json::to_string(&p).unwrap();
        let p2: LaunchPhase = serde_json::from_str(&s).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn launch_phase_silent_challenge_carries_attempt() {
        let p = LaunchPhase::SilentChallenge { attempt: 3 };
        let s = serde_json::to_string(&p).unwrap();
        let p2: LaunchPhase = serde_json::from_str(&s).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn launch_phase_refreshing_carries_reason() {
        let p = LaunchPhase::Refreshing {
            reason: RefreshReason::Got401,
        };
        let s = serde_json::to_string(&p).unwrap();
        let p2: LaunchPhase = serde_json::from_str(&s).unwrap();
        assert_eq!(p, p2);
    }

    #[test]
    fn launch_phase_offline_distinguishes_transient() {
        let transient = LaunchPhase::Offline { transient: true };
        let terminal = LaunchPhase::Offline { transient: false };
        assert_ne!(transient, terminal);
        let s = serde_json::to_string(&transient).unwrap();
        let p2: LaunchPhase = serde_json::from_str(&s).unwrap();
        assert_eq!(transient, p2);
    }

    #[test]
    fn launch_phase_wizard_at_carries_entry() {
        for entry in [
            LaunchWizardEntry::IdentityChoice,
            LaunchWizardEntry::HandleEntry,
            LaunchWizardEntry::InviteRequest,
            LaunchWizardEntry::ClaimCode,
        ] {
            let p = LaunchPhase::WizardAt { entry };
            let s = serde_json::to_string(&p).unwrap();
            let p2: LaunchPhase = serde_json::from_str(&s).unwrap();
            assert_eq!(p, p2);
        }
    }

    #[test]
    fn token_status_valid_carries_expires_at() {
        let t = TokenStatus::Valid {
            expires_at_secs: 1_700_000_000,
        };
        let s = serde_json::to_string(&t).unwrap();
        let t2: TokenStatus = serde_json::from_str(&s).unwrap();
        assert_eq!(t, t2);
    }

    #[test]
    fn refresh_reason_round_trip() {
        for r in [
            RefreshReason::ScheduledTtl,
            RefreshReason::Got401,
            RefreshReason::Manual,
        ] {
            let s = serde_json::to_string(&r).unwrap();
            let r2: RefreshReason = serde_json::from_str(&s).unwrap();
            assert_eq!(r, r2);
        }
    }

    #[test]
    fn launch_snapshot_round_trip_with_all_optionals_populated() {
        let snap = LaunchSnapshot {
            phase: LaunchPhase::Online,
            token: TokenStatus::Valid {
                expires_at_secs: 1_700_000_000,
            },
            last_error: Some("test error".into()),
            superseded_successor: Some("ab".repeat(32)),
            identity_fork: true,
            identity: Some(LaunchIdentity {
                handle: "alice".into(),
                domain: "nest.example".into(),
                tier: "free".into(),
            }),
            account_index_refusal: Some(crate::AccountIndexRefusal::NewerBuild {
                index_v: 2,
                index_min: 2,
                bin_v: 1,
            }),
            sign_in_refused: true,
            locked_until_secs: Some(1_700_086_400),
        };
        let s = serde_json::to_string(&snap).unwrap();
        let snap2: LaunchSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(snap, snap2);
    }

    /// A snapshot JSON without the field (web's wasm boundary crosses it as
    /// JSON; wasm and SPA ship together) still parses, reading the refusal as absent.
    #[test]
    fn launch_snapshot_without_sign_in_refused_parses_as_not_refused() {
        let mut v = serde_json::to_value(LaunchSnapshot::initial()).unwrap();
        v.as_object_mut().unwrap().remove("sign_in_refused");
        let snap: LaunchSnapshot = serde_json::from_value(v).unwrap();
        assert!(!snap.sign_in_refused);
    }

    #[test]
    fn launch_snapshot_initial_default_is_boot() {
        let snap = LaunchSnapshot::initial();
        assert_eq!(snap.phase, LaunchPhase::Boot);
        assert_eq!(snap.token, TokenStatus::None);
        assert_eq!(snap.last_error, None);
    }
}
