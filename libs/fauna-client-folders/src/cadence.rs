//! **Periodic-reconcile cadence** — a hard-coded constant, not a knob.
//!
//! Phase 5 of the folders re-model retired the per-folder scan-frequency
//! choice (`docs/goal/behavior/file-sync.md` § Config, the phase-5 block;
//! ratified 2026-08-19): the reconcile backstop is a hard-coded constant no
//! human chooses (`docs/goal/principles.md` § One configuration surface), and the
//! surviving user-facing time control is the nest place's snapshot policy.
//! Every reader — the linux in-process engines, the agent's hydration hosts, android's photo-backup worker — takes
//! [`DEFAULT_RESCAN_INTERVAL`] directly; the old `rescan_interval_for` row
//! resolve and the `rescan_interval_from_secs` carrier resolve are deleted.
//!
//! The wire, at-rest and cross-nest carrier copies of the old per-folder value
//! (`rescan_interval_secs`, `fauna.folders.schedule.set`) are removed
//! (`sync-engine-deployments.md` § Control Plane Principle, the phase-5 block).

use std::time::Duration;

/// The periodic full-reconcile cadence — the **only** value (phase 5's
/// de-knob; the name keeps its historical `DEFAULT_` prefix).
pub const DEFAULT_RESCAN_INTERVAL: Duration = Duration::from_secs(300);
