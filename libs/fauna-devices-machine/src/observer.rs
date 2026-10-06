//! Reactivity callback for the Devices page — clients re-render off a fresh
//! `DevicesSnapshot` on every tick. Invoked synchronously after every state
//! mutation (and forwarded from the embedded wizard's observer, so opening /
//! driving the wizard ticks the page too). Mirrors
//! `fauna_folders_machine::FolderWizardObserver`.

fauna_core::declare_snapshot_observer!(DevicesObserver);
