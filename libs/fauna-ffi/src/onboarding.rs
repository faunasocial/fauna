//! Re-exports the onboarding machine so its UniFFI exports surface in the
//! generated Swift / Kotlin / C# bindings. The machine itself lives in
//! libs/fauna-onboarding-machine; this file is a thin glue layer.

pub use fauna_onboarding_machine::{
    AdminNatModeMachine, AgeAttestationPlain, AgeClaimPlain, AgeNoncePlain, BoxRecoveryEntry,
    CapabilityPlain, DnsConfigState, DnsRecordPlain, FieldMetaPlain, FieldTypePlain,
    IdentityOrigin, OnboardingError, OnboardingMachine, OnboardingObserver, OnboardingStep,
    VpsConfigState, format_price, server_type_label,
};
