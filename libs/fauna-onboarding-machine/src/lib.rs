//! Handle-first onboarding state machine.
//!
//! The full design and UX spec are tracked internally.

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_onboarding_machine");

pub mod admin_nat_mode;
pub(crate) mod cancel;
pub mod error;
pub mod helpers;
/// The `hosted-auth` field's device-authorization flow, shared by the wizard
/// and the retire view.
pub(crate) mod hosted_auth;
pub mod machine;
pub mod nest_api;
pub mod observer;
pub mod outcome;
pub mod poll;
pub(crate) mod recovery_config;
/// Retiring an app-provisioned nest (`behavior/nest-retirement.md`).
pub mod retire;
pub mod snapshots;
pub mod state;

pub use admin_nat_mode::AdminNatModeMachine;
pub use error::OnboardingError;
pub use helpers::{
    format_price, handle_tld, qualify_reclaim_handle, server_type_allowed_for_mail,
    server_type_label,
};
/// The machine-free E2E-bridge dispatcher — the arms every app can drive
/// with or without a live machine (the nest-identity pin seed/read). See
/// [`machine::call_machine_free_method`].
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use machine::{FreeMethodOutcome, call_machine_free_method};
pub use machine::{OnboardingMachine, RecoveryEntryOutcome, recovery_entry_outcome_message};
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub use nest_api::FakeNestApi;
pub use nest_api::{NestApi, WsNestApi};
pub use observer::OnboardingObserver;
pub use outcome::WizardOutcome;
pub use poll::{AWAITING_DNS_POLL_MS, INVITE_RECHECK_POLL_MS};
pub use retire::{
    HeldDnsCredential, ManagedServerRow, NestRetireMachine, RetireInputs, RetirePhase,
    RetireSnapshot, RetireStep, RetireStepRow, StepState, TransferCodeState,
};
pub use snapshots::*;
pub use state::*;
