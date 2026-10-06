//! Reactivity callback for the ATProto login-plane settings surface — clients
//! re-render off a fresh [`crate::snapshots::AtprotoSettingsSnapshot`] on every
//! tick. Mirrors `fauna_labeler_catalog_machine::observer`.

fauna_core::declare_snapshot_observer!(AtprotoSettingsObserver);
