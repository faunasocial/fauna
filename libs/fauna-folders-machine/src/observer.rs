//! Reactivity callback — clients notify their view layer that they should
//! re-render. Invoked synchronously after every state mutation. Clients
//! debounce / throttle on their side if the framework demands it. Mirrors
//! `fauna_onboarding_machine::OnboardingObserver`.

fauna_core::declare_snapshot_observer!(FolderWizardObserver);
