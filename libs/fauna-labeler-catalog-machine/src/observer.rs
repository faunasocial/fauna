//! Reactivity callback for the labeler-catalog page — clients re-render off a
//! fresh `LabelerCatalogSnapshot` on every tick. Mirrors
//! `fauna_devices_machine::observer`.

fauna_core::declare_snapshot_observer!(LabelerCatalogObserver);
