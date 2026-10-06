//! Shared file-sync engine.
//!
//! The watcher, chunk pipeline, conflict-aware sync orchestration, and SQLite
//! state DB behind every deployment of client-side byte sync: app-side
//! in-process one-shots (photo ingress, restore walks, the segment-backup
//! driver), the per-user `fauna-sync-agent` hosting the always-resident
//! engines on all three desktops and in the headless deployment.
//! `app-guidelines.md` § 9 owns the deployment map.
//!
//! Crate layering (ruled 2026-08-03, `app-guidelines.md` § 9): [`db`] is
//! the crate's *floor*, not part of the engine — a standalone client-side
//! state-dir + SQLite layer with no code dependency on [`engine`] and a
//! deliberately thin dependency set (`fauna-core` + `rusqlite` + `anyhow`).
//! Linking this crate for the floor alone (account scoping, audits,
//! `device.db` reads) is the intended shape, not a smell; extraction of `db`
//! into its own crate is deferred until a production consumer wants it
//! without the engine's graph.
//!
//! See `docs/goal/behavior/file-sync.md` and
//! `docs/goal/architecture/apps/linux.md` § File Sync.

pub mod access_gate;
/// The client-side account-store lifecycle (W3 (account-data-plane.md § Workstreams)): the dedicated store thread,
/// its pump, and the `Send` handle apps hold (`account-data-plane.md` § The
/// account store → *The client-side lifecycle*).
#[cfg(feature = "account-runtime")]
pub mod account_runtime;
// The plane half — the account-state and group planes, the generation
// passes, the page walk, the preference surfaces and the plane-row modules —
// moved to the wasm-capable `fauna-account-plane` 2026-09-27
// (`account-data-plane.md` § The client-side lifecycle → *The trigger fired*,
// ruling (1)). Each is re-exported here under the gate it had, so every
// `crate::…` and `fauna_sync_engine::…` path reads unchanged.
pub use fauna_account_plane::account_state_plane;
pub use fauna_account_plane::attested_predecessors;
pub use fauna_account_plane::bind_leg;
pub use fauna_account_plane::linked_leg;
pub mod adaptive;
pub mod always_resident;
pub mod atomic_write;
/// The class-1 bulk half of the same plane — a fresh replica pulling one
/// scope's segment pairs off a real nest (`account-data-plane.md` § the
/// bootstrap contract, W2.2).
pub mod bootstrap_source;
pub mod causal;
/// The folder-key custody reader over a capability host's throwaway fleet
/// replica (`on-demand-files.md` § Shared sets on a capability host,
/// decision 1′).
pub mod cold_folder_keys;
pub use fauna_account_plane::cold_replica;
pub mod config;
pub mod conflict_resolver;
pub use fauna_account_plane::contact_overlay_rows;
pub use fauna_account_plane::content_scope_plane;
pub mod custodian_host;
pub mod custodian_pull;
pub mod custodian_store;
pub mod custody_leg;
pub use fauna_account_plane::atproto_identity_rows;
pub use fauna_account_plane::atproto_rows;
pub use fauna_account_plane::blessed_nest_rows;
pub use fauna_account_plane::custody_ceremony_rows;
pub use fauna_account_plane::custody_rows;
pub use fauna_account_plane::deployment_seed_recovery;
pub use fauna_account_plane::deployment_seed_rows;
pub use fauna_account_plane::dns_rows;
pub use fauna_account_plane::folder_key_rows;
pub use fauna_account_plane::follows_rows;
pub use fauna_account_plane::mail_rows;
pub use fauna_account_plane::nostr_confirmation_rows;
pub use fauna_account_plane::peer_anchor_rows;
#[cfg(feature = "account-runtime")]
pub use fauna_account_plane::preference_surfaces;
pub use fauna_account_plane::refused_change_rows;
/// The T10 principal-bundle carriage (W5.4a): the credential-slot attributes
/// beside the writer key — device authorization, backup key, retained
/// generation keys (`account-data-plane.md` § The store device principal).
#[cfg(feature = "account-runtime")]
pub mod principal_bundle;
/// Principal succession: the ceremony-time probe, the in-place
/// writer rotation, and the un-pushed-tail re-author
/// (`account-data-plane.md` § The store device principal → *Principal
/// succession after a device delete*).
#[cfg(feature = "account-runtime")]
pub mod principal_succession;
// The db floor was extracted to `libs/fauna-account-store` 2026-08-10 (its
// declared trigger — a production consumer wanting `db` without the engine —
// arrived as the account store; `app-guidelines.md` § crate layering). The
// re-export keeps every `fauna_sync_engine::db::…` path working verbatim.
pub use fauna_account_store::db;
// The W6 per-user account-store root (`StoreRoot`, `platform_state_base`) — same re-export convention as `db`, so apps and the
// sync agent reach the one path derivation without a new direct dep.
pub use fauna_account_store::root;
/// Decision 2's re-resolve edges: the row basis, the floor comparison, and the
/// engine's pre-seal hold (`on-demand-files.md` § Shared sets on a capability host).
pub mod binding_edge;
/// The ceremony's admission clock and its compile-gated e2e offset — the
/// `now` a receive-act expectation is minted and judged against. Same gate as
/// [`offline_share`], whose `now_fn` is its only production reader.
#[cfg(all(feature = "p2p-share", feature = "account-runtime"))]
pub mod ceremony_clock;
pub mod debouncer;
pub use fauna_account_plane::delegable_reclaim;
pub use fauna_account_plane::departure;
#[cfg(feature = "account-runtime")]
pub use fauna_account_plane::device_endpoints_writer;
pub mod engine;
pub mod engine_host;
#[cfg(feature = "engine-lifecycle")]
pub mod engine_lifecycle;
pub mod enumerate;
/// The store half of a device removal — moved to the wasm-capable plane
/// crate with ruling (2)'s slot seam; re-exported at its old path.
pub use fauna_account_plane::fleet_removal;
/// The exclusive-editing lease's client half (`file-sync.md` § Exclusive
/// editing): the posture a folder-list read installs, the write window an
/// upload pass takes, and the pure rule that decides whether this seat may
/// write a lease-governed folder at all.
pub mod folder_lease;
#[cfg(test)]
mod folder_lease_flush_test;
pub use fauna_account_plane::generation_escrow_recover;
#[cfg(test)]
use fauna_account_plane::generation_fixture_test_support;
pub use fauna_account_plane::generation_let_go;
pub use fauna_account_plane::generation_mint;
pub use fauna_account_plane::generation_reclaim;
pub use fauna_account_plane::generation_reescrow;
pub use fauna_account_plane::generation_tip;
pub use fauna_account_plane::generation_topup;
pub use fauna_account_plane::generation_unkeyable;
/// The group plane's authority-device severance — the revocation publisher,
/// the re-admissions it owes and the first production re-mint trigger
/// (`account-data-taxonomy.md` § The recipient-set scheme → *Severance, per
/// axis* → *An authority device's removal*). Gated with the bridge the
/// account driver implies; its re-admission source is the
/// `fauna.state.group-share-ceremony` record — the deliver snapshot, at the
/// same entry id under the live device's own cell — never the merged cell.
#[cfg(feature = "preference-store")]
pub use fauna_account_plane::group_authority_revocation;
pub use fauna_account_plane::publish_diff;
/// The peer witness door's production evaluator — every held group scope
/// folded from this replica's own store, pump-fed into the share plane's
/// admit exchange (`account-data-taxonomy.md` § The recipient-set scheme →
/// *Severance, per axis*). Gated with the share leg whose seam it implements.
#[cfg(feature = "p2p-share")]
pub mod group_roster_door;
pub use fauna_account_plane::backup_rows;
pub use fauna_account_plane::group_scope_view;
pub use fauna_account_plane::group_share_rows;
pub use fauna_account_plane::group_state_plane;
pub use fauna_account_plane::succession_ledger_rows;
pub mod hydrator;
pub mod ignore;
pub mod nest_api;
pub mod nest_client;
/// The T1 body-rendered browse trigger — moved to the plane crate with the
/// driver whose handle is its only door; re-exported at its old path.
pub use fauna_account_plane::observation_intake;
/// The offline co-present share ceremony's orchestration and records —
/// what every app's folders page drives once its own bind door has handed
/// back a seat. Same gate as [`share_glue`]: the ceremony types ride
/// `p2p-share` and the group doors ride the runtime.
#[cfg(all(feature = "p2p-share", feature = "account-runtime"))]
pub mod offline_share;
pub use fauna_account_plane::outbox;
pub mod own_novel_in_flight;
#[cfg(feature = "account-runtime")]
pub use fauna_account_plane::p2p_participation;
use fauna_account_plane::page_walk;
/// The frontier writer-count ceiling the page loop enforces. Re-exported
/// because the nest derives the admin-settable `AdminTier::max_devices` bound
/// from it — the relationship the ceiling's own sizing comment assumes
/// (`account-sync-plane.md` § Feeds and cursors).
pub use page_walk::MAX_FRONTIER_WRITERS;
#[cfg(feature = "account-runtime")]
pub mod peer_leg;
#[cfg(all(test, feature = "p2p-share"))]
mod peer_share_ingest_test;
#[cfg(all(test, feature = "p2p-share"))]
mod peer_share_serve_test;
#[cfg(feature = "p2p-share")]
pub mod peer_share_store;
/// The relay-serving seat: a host of resident engines announces their folders
/// on its WS-RPC connection and answers the nest's asks (`file-sync.md`
/// § Relay serving).
pub mod relay_seat;
/// The engine's one serve core: a stored chunk re-derived from the body this
/// seat holds (`file-sync.md` § Relay serving).
pub mod serve_core;
/// Where the serve core reads a body's plaintext: a bound tree, or an
/// on-demand replica's two roots. Not feature-gated — relay serving reads
/// through it too.
pub mod share_body;
/// The share leg's app **driver** — the loop, the spec join and the surface
/// readings every app's share glue would otherwise re-derive privately (the
/// join among them). Same gate as the pump it composes.
#[cfg(all(feature = "p2p-share", feature = "account-runtime"))]
pub mod share_glue;
/// Which pulled bodies a replica lands, and where — the share plane's landing
/// policy as pure functions.
#[cfg(feature = "p2p-share")]
pub mod share_landing;
/// A raw read of one shared set from one peer — the compile-gated e2e probe
/// behind "a non-member gets nothing readable from your device" (the same
/// probe, run from a member, is its control).
#[cfg(all(
    feature = "p2p-share",
    feature = "account-runtime",
    any(debug_assertions, feature = "e2e-agent")
))]
pub mod share_probe;
/// The share leg's app-side pump (slice E) — needs both the share plane and
/// the runtime (the node it dials through rides the `account-runtime` dep).
#[cfg(all(feature = "p2p-share", feature = "account-runtime"))]
pub mod share_pump;
#[cfg(all(test, feature = "p2p-share"))]
mod share_replica_test;
/// What this process's share plane served, per path, plus the hold that parks
/// the next body serve — the compile-gated e2e observables for a resumed
/// transfer.
#[cfg(feature = "p2p-share")]
pub mod share_serve_tally;
/// Path-traversal guard — moved to `fauna-core` 2026-07-16 so the shared
/// client-side file-download walk (`fauna_core::file_download`, which wasm
/// compiles and this crate cannot) guards its own paths. Re-exported so this
/// crate's `crate::path_guard::…` call sites read unchanged.
pub use fauna_core::path_guard;
pub mod placeholder;
pub use fauna_account_plane::preference_put;
/// Three-valued on-disk presence: the destructive side never reads *"I could
/// not stat it"* as *"the user deleted it"*.
pub mod presence;
pub mod progress;
pub mod provider_face;
pub mod reseed;
pub use fauna_account_plane::scope_set;
pub mod seal;
pub use fauna_account_plane::seen_set_producer;
pub mod segment_backup;
pub use fauna_account_plane::subscription_rows;
// Extracted with the db floor (see the `db` re-export above): these modules
// are inherent-`impl` satellites of `SyncDb` — pure DB-floor logic that the
// orphan rule requires to live beside the type. Re-exported so no path breaks.
pub use fauna_account_store::succession_drain;
pub use fauna_account_store::succession_progress;
pub mod transfer;
pub mod transfer_worker;
pub mod watcher;
pub mod write_token_bearer;

pub use hydrator::FileHydrator;

#[cfg(test)]
mod access_revoked_test;
#[cfg(test)]
mod anchor_accounting_test;
#[cfg(test)]
mod backfill_thumbnail_test;
#[cfg(all(test, feature = "engine-lifecycle"))]
mod build_engine_retired_custody_test;
#[cfg(test)]
mod catchup_failed_report_test;
#[cfg(test)]
mod connected_arm_heal_test;
#[cfg(test)]
mod create_arm_local_conflict_test;
#[cfg(test)]
mod cross_nest_reader_roster_test;
#[cfg(test)]
mod delete_ack_test;
#[cfg(test)]
mod deposit_adoption_test;
#[cfg(test)]
mod download_file_bytes_test;
#[cfg(test)]
mod enumerate_test;
#[cfg(test)]
mod head_rejudge_test;
#[cfg(test)]
mod keep_local_resolution_test;
#[cfg(test)]
mod log_redaction_test;
#[cfg(test)]
mod mass_delete_floor_test;
#[cfg(test)]
mod merge_convergence_test;
#[cfg(test)]
mod off_disk_placeholder_test;
#[cfg(test)]
mod offline_placeholder_delete_test;
#[cfg(test)]
mod offline_publication_hold_test;
#[cfg(test)]
mod populate_placeholders_test;
#[cfg(test)]
mod predecessor_signed_rows_test;
#[cfg(test)]
mod pull_remote_changes_test;
#[cfg(test)]
mod readopt_row_identity_test;
#[cfg(test)]
mod record_head_commit_wiring_test;
#[cfg(test)]
mod relay_serve_test;
#[cfg(test)]
mod resident_placeholder_test;
#[cfg(test)]
mod seal_recorded_path_test;
#[cfg(test)]
mod superseded_own_record_test;
// The mock nest (`MockNest`/`BlobStore`) other crates' tests reach through
// `test-helpers`. Gate deliberately without rule (a)'s `debug_assertions` arm —
// the module's own docs carry the reason, and it matches `nest_api::FakeSyncControl`'s
// gate in this same crate (e2e-automation-surface-gating.md § convention 15).
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_support;
#[cfg(test)]
mod upload_thumbnail_test;
