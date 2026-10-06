//! Reactivity callback for the Backups page — clients re-render off a fresh
//! `BackupsSnapshot` on every tick. Invoked synchronously after every state
//! mutation. Mirrors `fauna_devices_machine::DevicesObserver`.

fauna_core::declare_snapshot_observer!(BackupsObserver);
