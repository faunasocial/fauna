//! Reactivity callback for the connected-apps page — apps re-render off a
//! fresh `ConnectedAppsSnapshot` on every tick. Mirrors
//! `fauna_labeler_catalog_machine::observer`.

fauna_core::declare_snapshot_observer!(ConnectedAppsObserver);
