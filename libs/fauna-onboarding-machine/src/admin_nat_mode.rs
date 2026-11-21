//! The **admin-panel NAT-mode control** — the post-onboarding change surface
//! for the nest's NAT axis (`public` / `private`), rendered on **Admin → Nest**
//! as `admin-nest-nat-mode-{public-radio,private-radio,save-button,status}`
//! (`docs/goal/behavior/admin.md` § Nest → NAT-mode control).
//!
//! This is the second consumer of the exact commit ceremony the wizard's
//! `nat_mode_choice` page drives (`machine.rs::submit_nat_mode_choice`): the
//! same [`NestApi`] seam (`probe_setup_status` read, `submit_nat_mode` write),
//! the same signed body ([`crate::machine::build_signed_nat_mode_body`]), the
//! same [`NatModeSnapshot`] render shape — so the wizard page and the admin
//! page can never drift. `fauna.setup.nat_mode` is a **mutable upsert**
//! authorized by the *payload signature* (the committing admin), so it needs
//! no bearer connection: this machine rides the same pre-identity WS-RPC
//! transport ([`WsNestApi`]) the wizard uses, opening a fresh connection per
//! call.
//!
//! Lifecycle: `hydrate()` reads `fauna.setup.status`'s `node_mode` and
//! pre-selects it (absent ⇒ `public`, the nest's absent-row default);
//! `select()` flips the pending radio; `submit()` signs and commits. There is
//! **no defer affordance** — navigating away is the defer (admin.md). Unlike
//! the wizard, a successful save keeps `submit_enabled == true`: the set is
//! mutable and the page persists, so the admin may flip again immediately.
//!
//! Unknown is not possible (`docs/goal/ui/README.md` § Offer only what is
//! possible, rule 5): until `hydrate()` has read the nest's current mode, the
//! radios and save answer *blocked* through [`AdminNatModeMachine::select_possibility`]
//! and [`AdminNatModeMachine::save_possibility`], and `select()` / `submit()`
//! refuse. A failed read leaves them blocked with the load failure: saving the
//! pre-selected default `public` over a mode the page never read would be a
//! guess written to the nest.
//!
//! Status message keys are the `admin.nest_page.nat_mode_*` i18n group; the
//! idle/saved texts carry the live-vs-restart-applied caveat (a flip
//! live-re-evaluates the MTA supervisor + MDA bind; ACME/STUN re-gate at the
//! next restart — `deployment-home-with-public-relay.md` § Implementation
//! status, Slice 1).

use std::sync::{Arc, Mutex};

use fauna_core::possibility::{Possibility, ReadState};
use fauna_core::secret::SecretString;

use crate::nest_api::{NatModeError, NestApi, ProbeError};
use crate::snapshots::nat_mode::{NatModeSnapshot, NatModeState};
use crate::state::{LocalizedText, NodeMode};

fn text(key: &str) -> LocalizedText {
    LocalizedText {
        key: key.into(),
        args: Default::default(),
    }
}

fn text_with_cause(key: &str, cause: String) -> LocalizedText {
    LocalizedText {
        key: key.into(),
        args: [("cause".to_string(), cause)].into_iter().collect(),
    }
}

/// One instance per admin-nest page visit. Holds the rendered
/// [`NatModeSnapshot`]; drives the shared [`NestApi`] seam. Dispatch-style
/// like the admin policy machines: the view awaits each action and re-reads
/// [`Self::snapshot`] — no observer.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
#[derive(Debug)]
pub struct AdminNatModeMachine {
    nest_api: Arc<dyn NestApi>,
    nest_url: String,
    secret_hex: SecretString,
    snap: Mutex<NatModeSnapshot>,
    /// Where `hydrate()`'s read of the current mode stands. Only a resolved
    /// read makes the radios and save possible (rule 5).
    read: Mutex<ReadState>,
}

impl AdminNatModeMachine {
    /// Test/e2e constructor over an explicit [`NestApi`] (the fake).
    pub fn with_nest_api(
        nest_api: Arc<dyn NestApi>,
        nest_url: String,
        secret_hex: SecretString,
    ) -> Arc<Self> {
        Arc::new(Self {
            nest_api,
            nest_url,
            secret_hex,
            snap: Mutex::new(NatModeSnapshot {
                message: text("admin.nest_page.nat_mode_loading"),
                ..NatModeSnapshot::idle()
            }),
            read: Mutex::new(ReadState::Pending),
        })
    }

    fn read_state(&self) -> ReadState {
        *self.read.lock().expect("read mutex")
    }

    fn set_read(&self, read: ReadState) {
        *self.read.lock().expect("read mutex") = read;
    }

    fn set_snap(&self, f: impl FnOnce(&mut NatModeSnapshot)) {
        f(&mut self.snap.lock().expect("snapshot mutex"));
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl AdminNatModeMachine {
    /// Production constructor: the pre-identity WS-RPC transport against
    /// `nest_url`, signing with the admin's identity `secret_hex` (the same
    /// secret the client authenticates with — the payload signature is the
    /// authorization, no bearer involved).
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new(nest_url: String, secret_hex: SecretString) -> Arc<Self> {
        let nest_api: Arc<dyn NestApi> = Arc::new(crate::nest_api::WsNestApi::new(
            None,
            Arc::new(std::sync::RwLock::new(None)),
            Arc::new(std::sync::RwLock::new(None)),
        ));
        Self::with_nest_api(nest_api, nest_url, secret_hex)
    }

    /// The rendered state. Pure read.
    pub fn snapshot(&self) -> NatModeSnapshot {
        self.snap.lock().expect("snapshot mutex").clone()
    }

    /// Whether the radios (`admin-nest-nat-mode-{public,private}-radio`) are
    /// live: only once the current mode has been read (rule 5) — pending says
    /// still loading, a failed read says it failed.
    pub fn select_possibility(&self) -> Possibility {
        Possibility::from_read(self.read_state())
    }

    /// Whether `admin-nest-nat-mode-save-button` is live: the read resolved
    /// (rule 5) and no save is on its way (`submit_enabled`, cleared while
    /// one is in flight). `submit()` asks the same answer.
    pub fn save_possibility(&self) -> Possibility {
        let in_flight = !self.snap.lock().expect("snapshot mutex").submit_enabled;
        Possibility::from_read(self.read_state()).and(if in_flight {
            Possibility::blocked(LocalizedText::key("common.in_flight"))
        } else {
            Possibility::Possible
        })
    }

    /// Radio click (`admin-nest-nat-mode-{public,private}-radio`). Recovers
    /// from `Error`/`Done` back to `Choosing`; save stays enabled. Refused
    /// (nothing changes) while [`Self::select_possibility`] is blocked.
    pub fn select(&self, mode: NodeMode) {
        if !self.select_possibility().is_possible() {
            return;
        }
        self.set_snap(|s| {
            s.selected_mode = mode;
            s.state = NatModeState::Choosing;
            s.message = text("admin.nest_page.nat_mode_choosing");
            s.submit_enabled = true;
        });
    }
}

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl AdminNatModeMachine {
    /// Page load: read `fauna.setup.status` and pre-select the current
    /// `node_mode` (an unknown spelling ⇒ `public`). A read failure surfaces on
    /// the status line and leaves the radios and save BLOCKED: the page does
    /// not know the nest's mode, and the pre-selected default is a guess
    /// (`docs/goal/ui/README.md` § Offer only what is possible, rule 5). A
    /// later `hydrate()` (the page revisited) can still resolve it.
    pub async fn hydrate(&self) {
        if self.read_state() != ReadState::Resolved {
            self.set_read(ReadState::Pending);
        }
        self.set_snap(|s| {
            s.state = NatModeState::Submitting; // renders as "loading"; save disabled
            s.message = text("admin.nest_page.nat_mode_loading");
            s.submit_enabled = false;
        });
        match self.nest_api.probe_setup_status(&self.nest_url).await {
            Ok(status) => {
                self.set_read(ReadState::Resolved);
                self.set_snap(|s| {
                    s.selected_mode = status.node_mode.unwrap_or(NodeMode::Public);
                    s.state = NatModeState::Choosing;
                    s.message = text("admin.nest_page.nat_mode_choosing");
                    s.submit_enabled = true;
                })
            }
            Err(e) => {
                // A nest too new for this app (its 426) is the one probe
                // failure no retry clears (version-compatibility.md Dim 4).
                let transient = !matches!(e, ProbeError::AppOutdated { .. });
                let cause = match e {
                    ProbeError::Transient { reason } | ProbeError::AppOutdated { reason } => reason,
                    other => format!("{other:?}"),
                };
                // A re-read that fails after an earlier one resolved keeps the
                // mode that read gave; only a page that never read stays blocked.
                let resolved = self.read_state() == ReadState::Resolved;
                if !resolved {
                    self.set_read(ReadState::Failed);
                }
                self.set_snap(|s| {
                    s.state = NatModeState::Error {
                        transient,
                        cause: cause.clone(),
                    };
                    s.message = text_with_cause("admin.nest_page.nat_mode_error_load", cause);
                    s.submit_enabled = resolved;
                });
            }
        }
    }

    /// Save (`admin-nest-nat-mode-save-button`): sign the canonical payload
    /// and commit the selected mode via the mutable `fauna.setup.nat_mode`.
    /// Success lands `Done` with save **re-enabled** (the admin may flip
    /// again); failures land `Error` with save enabled (resubmit is always
    /// allowed).
    /// Refused (nothing is sent) while [`Self::save_possibility`] is blocked.
    pub async fn submit(&self) {
        if !self.save_possibility().is_possible() {
            return;
        }
        let selected_mode = {
            let mut s = self.snap.lock().expect("snapshot mutex");
            s.state = NatModeState::Submitting;
            s.message = text("admin.nest_page.nat_mode_submitting");
            s.submit_enabled = false;
            s.selected_mode
        };
        // The impl signs on the commit connection (the wizard's shared
        // ceremony — `build_signed_nat_mode_body` via
        // `WsRpcNestApi::submit_nat_mode`, which binds the nest identity it
        // learns there). An unparseable secret comes back as the terminal
        // `Invalid`.
        match self
            .nest_api
            .submit_nat_mode(&self.nest_url, &self.secret_hex, selected_mode)
            .await
        {
            Ok(()) => self.set_snap(|s| {
                s.state = NatModeState::Done;
                s.message = text("admin.nest_page.nat_mode_saved");
                s.submit_enabled = true;
            }),
            Err(NatModeError::Invalid { reason }) => self.set_snap(|s| {
                s.state = NatModeState::Error {
                    transient: false,
                    cause: reason.clone(),
                };
                s.message = text_with_cause("admin.nest_page.nat_mode_error_terminal", reason);
                s.submit_enabled = true;
            }),
            Err(NatModeError::Transient { cause }) => self.set_snap(|s| {
                s.state = NatModeState::Error {
                    transient: true,
                    cause: cause.clone(),
                };
                s.message = text_with_cause("admin.nest_page.nat_mode_error_transient", cause);
                s.submit_enabled = true;
            }),
            // The typed identity verdict (security.md § Pre-claim surfacing):
            // terminal, never a resubmit invitation.
            Err(NatModeError::IdentityMismatch { reason }) => self.set_snap(|s| {
                s.state = NatModeState::Error {
                    transient: false,
                    cause: reason.clone(),
                };
                s.message = text_with_cause("admin.nest_page.nat_mode_error_terminal", reason);
                s.submit_enabled = true;
            }),
        }
    }
}
