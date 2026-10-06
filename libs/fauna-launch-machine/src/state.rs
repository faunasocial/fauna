//! Internal state. Carries data the public `LaunchPhase` doesn't expose
//! (secret bytes, in-flight bearer, etc.) so the snapshot stays minimal.
//!
//! See `machine.rs` for the public surface; `snapshots::LaunchPhase` for
//! what clients see via `LaunchMachine::snapshot()`.

use crate::snapshots::{LaunchPhase, LaunchWizardEntry, RefreshReason};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum State {
    Boot,
    Hydrating,
    /// Silent challenge in flight. Carries the secret + nest_url so the
    /// challenge runner doesn't need to re-read persistence.
    SilentChallenge {
        #[allow(dead_code)] // populated in routing
        secret: Vec<u8>,
        #[allow(dead_code)]
        nest_url: String,
        attempt: u32,
    },
    /// Authenticated; a bearer-token refresh is in flight.
    #[allow(dead_code)] // entered by the token-refresh path (machine.rs)
    Refreshing {
        secret: Vec<u8>,
        reason: RefreshReason,
    },
    /// Authenticated, token valid.
    #[allow(dead_code)] // entered after silent challenge or refresh succeeds
    Online {
        secret: Vec<u8>,
        bearer: String,
        /// The session id the live bearer names — carried so the machine's
        /// own-session set and the live token can never disagree about which
        /// row is current (`docs/goal/behavior/devices.md` § The client's own
        /// session). Never reaches `LaunchPhase`: like `bearer` and `secret`
        /// it is credential-adjacent state the snapshot does not expose.
        token_id: String,
        expires_at_secs: u64,
    },
    /// Network or terminal failure. `transient: true` shows retry UI;
    /// `transient: false` is terminal (e.g. account locked).
    #[allow(dead_code)] // entered by the failure-mode handlers (machine.rs)
    Offline {
        transient: bool,
    },
    /// Drop-to-wizard at the named entry point.
    WizardAt {
        entry: LaunchWizardEntry,
    },
    /// The nest's pinned deployment identity changed (or was withdrawn).
    /// Blocked until the user explicitly re-trusts or falls through to the
    /// wizard. Carries the secret + nest_url so `trust_nest_identity()` can
    /// forget the pin and re-run the silent challenge without re-reading
    /// persistence.
    IdentityChanged {
        secret: Vec<u8>,
        nest_url: String,
        pinned_hex: String,
        seen_hex: Option<String>,
        /// Rotation-chain fork evidence (`box-recovery.md` § Client
        /// acceptance): `trust_nest_identity()` REFUSES while this is set —
        /// there is no re-trust on the fork surface. Projected to the
        /// snapshot's additive `identity_fork` field (never a `LaunchPhase`
        /// change — see `State::Superseded`'s docs for the exhaustive-switch
        /// reasoning).
        fork: bool,
    },
    /// This identity was **succeeded** — the account belongs to
    /// `new_actor_id_hex` now (`identity-succession.md` § Propagation → *Own
    /// device fleet*). Terminal: no retry can clear it, because the old key
    /// still signs valid bytes that authorize nothing.
    ///
    /// Projects to `LaunchPhase::Offline { transient: false }` — the existing
    /// terminal, no-retry phase — rather than a new `LaunchPhase` variant,
    /// deliberately. `LaunchPhase` is UniFFI-exported and switched over
    /// **exhaustively with no default arm** by macOS, iOS and android, none of
    /// which this machine's CI can compile; a new variant is a build break on
    /// two machines for a surface those apps do not render yet. The successor
    /// instead rides an *additive* field on `LaunchSnapshot` (a UniFFI Record —
    /// additive fields break no exhaustive switch), which is the same
    /// side-channel shape `fauna-client`'s `SupersededLatch` chose at the mint
    /// boundary for the same reason. Apps that do not read it still stop
    /// retrying and still show the refusal's message; tui and linux read it and
    /// render the import affordance.
    Superseded {
        new_actor_id_hex: String,
    },
    /// A nest this app had signed in to before answered the opaque
    /// `fauna.auth.not_registered`, and the nest is claimed — it no longer
    /// signs this identity in (suspended or removed; the app cannot tell, by
    /// design). `onboarding.md` § App-launch routing → *the previously-signed-in
    /// row*, reached from the launch arm and the mid-session refresh arm alike.
    ///
    /// Projects to `LaunchPhase::Offline { transient: false }` plus the
    /// additive `LaunchSnapshot::sign_in_refused` side channel — the same
    /// shape as [`State::Superseded`], for the same exhaustive-switch reason.
    /// Terminal for the machine's own scheduling, but **the one terminal state
    /// whose `retry_silent_challenge()` is honoured**: the remedy is the
    /// admin's restore, a button on *their* app, after which a retry is the
    /// user's way back in.
    SignInRefused,
    /// `fauna.auth.account_locked` — the account is locked out until
    /// `locked_until_secs` (Unix seconds; `devices.md` § The locked state).
    /// Terminal until then: no retry can clear it before its time.
    ///
    /// Projects to `LaunchPhase::Offline { transient: false }` plus the additive
    /// `LaunchSnapshot::locked_until_secs` side channel — the shape of
    /// [`State::Superseded`], for the same exhaustive-switch reason. Not
    /// retry-able through `retry_silent_challenge()`: the one refresh the
    /// machine arms for `locked_until` when it parks here is the way out
    /// (`LaunchMachine::arm_lock_refresh`).
    Locked {
        locked_until_secs: u64,
    },
}

impl State {
    /// Project the internal state into the public `LaunchPhase`. Strips
    /// secret bytes and bearer tokens so observers / serialized snapshots
    /// don't leak credentials.
    pub(crate) fn to_phase(&self) -> LaunchPhase {
        match self {
            State::Boot => LaunchPhase::Boot,
            State::Hydrating => LaunchPhase::Hydrating,
            State::SilentChallenge { attempt, .. } => {
                LaunchPhase::SilentChallenge { attempt: *attempt }
            }
            State::Refreshing { reason, .. } => LaunchPhase::Refreshing { reason: *reason },
            State::Online { .. } => LaunchPhase::Online,
            State::Offline { transient } => LaunchPhase::Offline {
                transient: *transient,
            },
            State::WizardAt { entry } => LaunchPhase::WizardAt { entry: *entry },
            State::IdentityChanged {
                pinned_hex,
                seen_hex,
                ..
            } => LaunchPhase::IdentityChanged {
                pinned_hex: pinned_hex.clone(),
                seen_hex: seen_hex.clone(),
            },
            // Terminal, non-retry — see the variant's own docs for why this is
            // the existing phase plus a snapshot side channel, not a new phase.
            State::Superseded { .. } => LaunchPhase::Offline { transient: false },
            // Same shape, same reason — the verdict rides the snapshot's
            // `sign_in_refused` side channel.
            State::SignInRefused => LaunchPhase::Offline { transient: false },
            // Same shape — the unlock time rides `locked_until_secs`.
            State::Locked { .. } => LaunchPhase::Offline { transient: false },
        }
    }

    /// The unlock time (Unix seconds) when the machine is parked on a
    /// `fauna.auth.account_locked` refusal. `None` in every other state.
    pub(crate) fn locked_until_secs(&self) -> Option<u64> {
        match self {
            State::Locked { locked_until_secs } => Some(*locked_until_secs),
            _ => None,
        }
    }

    /// True when the machine is parked on the previously-signed-in row's
    /// refusal (`LaunchSnapshot::sign_in_refused`). `false` in every other state.
    pub(crate) fn sign_in_refused(&self) -> bool {
        matches!(self, State::SignInRefused)
    }

    /// The successor this identity was succeeded by, when the machine has
    /// latched a `fauna.auth.superseded` refusal. `None` in every other state.
    ///
    /// **Claimed, not proven** — the caller verifies it against the registration
    /// chain (`fauna_client_recovery::resolve_successor`) before presenting it
    /// as fact; the nest is enforcer and distributor, never authorizer
    /// (`identity-succession.md` § Propagation).
    pub(crate) fn superseded_successor(&self) -> Option<String> {
        match self {
            State::Superseded { new_actor_id_hex } => Some(new_actor_id_hex.clone()),
            _ => None,
        }
    }

    /// True when the current identity-changed block is rotation-chain fork
    /// evidence (no re-trust surface). `false` in every other state.
    pub(crate) fn identity_fork(&self) -> bool {
        matches!(self, State::IdentityChanged { fork: true, .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn online_to_phase_strips_credentials() {
        let s = State::Online {
            secret: vec![0xaa; 32],
            bearer: "secret-token".into(),
            token_id: "0011223344556677".into(),
            expires_at_secs: 1_700_000_000,
        };
        let p = s.to_phase();
        assert_eq!(p, LaunchPhase::Online);
        // Verify no bearer field leaks via serde
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("secret-token"));
    }

    #[test]
    fn silent_challenge_to_phase_carries_attempt_only() {
        let s = State::SilentChallenge {
            secret: vec![0xaa; 32],
            nest_url: "https://nest.example".into(),
            attempt: 2,
        };
        let p = s.to_phase();
        assert_eq!(p, LaunchPhase::SilentChallenge { attempt: 2 });
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("nest.example"));
    }
}
