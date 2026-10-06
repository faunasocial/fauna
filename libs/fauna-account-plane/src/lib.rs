//! The account data plane's wasm-capable client half.
//!
//! Ruling (1) of web's account-plane hosting (`account-data-plane.md` § The
//! client-side lifecycle → *The trigger fired*): the plane half of
//! `fauna-sync-engine` — the account-state and group planes, the generation
//! passes, the page walk, the preference put and the plane-row modules
//! beside them — lives here, over the `fauna-account-store` floor, and
//! compiles for wasm32 so web rides the same store API (`account-data-plane.md`
//! § The account store → *Physical realization*). `fauna-sync-engine`
//! re-exports every module at its old path, so `fauna_sync_engine::account_state_plane::…`
//! and every other engine path read unchanged.
//!
//! Ruling (2) — the pump split — put the driver here too (`account_driver`,
//! under the `account-driver` feature): the command service, the pass and
//! the serve loop, with the credential slot, the native legs and the election
//! behind seams (`principal_custody`, `account_driver::HostLegs`,
//! `host_legs::EngineElection`). With it came `fleet_removal`,
//! `observation_intake` (its writer's only door is the driver's handle, by a
//! `pub(crate)` the security review ruled — the door and the writer share a
//! crate), `group_authority_revocation` and the succession tail
//! (`succession_tail`). What stays native in the engine is what the seams
//! name: the store thread and its runtime, the slot (`principal_bundle`), the
//! succession probe and heal (`principal_succession`), the peer and custody
//! legs, and the file- and SQLite-bound stores.
//!
//! The crate's public surface IS the engine's consumption: items the engine
//! reached as `pub(crate)` before the extraction are `pub` here, each named
//! for what it is.
//!
//! No wall clock beyond `fauna_core::data::Timestamp` — `SystemTime` and
//! `Instant` panic on wasm32.

/// The account driver — the command service, the pass and the serve loop
/// (`account-data-plane.md` § The client-side lifecycle → *The trigger fired*,
/// ruling (2)); hosted natively by `fauna_sync_engine::account_runtime`, on
/// web as a `spawn_local` task.
#[cfg(feature = "account-driver")]
pub mod account_driver;
/// The class-2 client leg of the generalized account-data feed — publish,
/// walk, merge (`account-sync-plane.md` § Feeds and cursors).
pub mod account_state_plane;
pub mod atproto_identity_rows;
pub mod atproto_rows;
/// The attested predecessor identities a runtime is handed — the ids and
/// their delegable schedules, as one value.
pub mod attested_predecessors;
pub mod backup_rows;
pub mod bind_leg;
pub mod blessed_nest_rows;
/// The throwaway fleet-scope replica a process that hosts no account store
/// reads fleet-only kinds through (`on-demand-files.md` § Shared sets on a
/// capability host, decision 1′).
pub mod cold_replica;
pub mod contact_overlay_rows;
/// The class-1 leg of a content scope: the feed walk that follows the bulk
/// bootstrap half (`account-data-plane.md` § the bootstrap contract).
pub mod content_scope_plane;
pub mod custody_ceremony_rows;
pub mod custody_rows;
/// The delegable scope's retire behind the succession carry: a pass step that
/// retires each predecessor row once a listed member row carries its item
/// (`succession-aftermath.md` § Re-key scope).
pub mod delegable_reclaim;
pub mod departure;
pub mod deployment_seed_recovery;
pub mod deployment_seed_rows;
pub mod device_endpoints_writer;
pub mod dns_rows;
/// Who may author a row of a third-party kind — the replica-side admission
/// of a principal writer (`third-party-kinds.md` § Principal write authority).
pub mod ext_writers;
/// The store half of a device removal, and its completion leg over the slot
/// seam (`account-data-taxonomy.md` § The generation machinery →
/// *Fleet-scope reclamation*, clause (4)).
pub mod fleet_removal;
pub mod folder_key_rows;
pub mod follows_rows;
/// The escrow-recovery pass — a seed-holding device keys, from the holder's
/// escrow wrap, every live generation it cannot key otherwise
/// (`account-data-taxonomy.md` § The generation machinery → *Escrow
/// recovery*).
pub mod generation_escrow_recover;
/// The generation-writer test fixture, shared with other crates' tests
/// through `test-helpers`.
#[cfg(any(test, feature = "test-helpers"))]
pub mod generation_fixture_test_support;
/// The let-go — a dead generation's rows retired by the user's confirmed act,
/// and by nothing else (`account-data-taxonomy.md` § The generation machinery
/// → *Fleet-scope reclamation*, clause (3)(j)).
pub mod generation_let_go;
pub mod generation_mint;
/// The fleet-scope reclamation pass — the `fauna.state.device-reach` writer
/// and the `fauna.account.state.retire` caller that keep the fleet scope's
/// live-entry count flat across generations and removals
/// (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
/// reclamation*).
pub mod generation_reclaim;
/// The re-escrow pass — the generation axis's succession rider: every live
/// generation this device keys is escrowed to the identity this runtime is.
/// It deposits and never mints (`config-dissolution.md` § The `__config`
/// dissolution schedule → *The closure order*, step (4)).
pub mod generation_reescrow;
/// The live-row read the generation passes share.
pub mod generation_store;
pub mod generation_tip;
/// The top-up self-heal pass — the `fauna.state.generation-wrap` writer that
/// un-partitions reads for a device a mint left out (`account-data-plane.md`
/// § The generation machinery, the top-up kind).
pub mod generation_topup;
/// The target-authored "cannot key" signal pass — the
/// `fauna.state.generation-unkeyable` writer that clears the one durable
/// forged-partition case the hardening left
/// (`account-data-plane.md` § The generation machinery).
pub mod generation_unkeyable;
/// The group plane's authority-device severance — the revocation publisher,
/// the re-admissions it owes and the first production re-mint trigger
/// (`account-data-taxonomy.md` § The recipient-set scheme → *Severance, per
/// axis* → *An authority device's removal*). Gated with the feature the
/// account driver implies — its one caller is the driver's pass; its
/// re-admission source is the `fauna.state.group-share-ceremony` record on
/// this replica's own store.
#[cfg(feature = "preference-store")]
pub mod group_authority_revocation;
pub mod group_scope_view;
pub mod group_share_rows;
pub mod group_state_plane;
/// The host seams' shared vocabulary: the native legs' report shapes and the
/// engine-singleton election.
pub mod host_legs;
/// The admitted-kinds overlay's carriage (`fauna.state.kind-manifest`) and its
/// re-verifying read fold (`third-party-kinds.md` § The kinds vocabulary).
pub mod kind_manifest_rows;
/// The secondary leg — a linked nest carrying the `account_replica`
/// capability, completed by every seed-holding runtime over a second
/// owner-authenticated connection (`account-sync-plane.md` § The bind leg,
/// ruling 4).
pub mod linked_leg;
pub mod mail_rows;
pub mod nostr_confirmation_rows;
/// The T1 body-rendered browse trigger: the shared intake that turns an app's
/// "this body was displayed" report into a seen-set membership
/// (`account-data-plane.md` § The replica boundary → T1). Its writer is
/// crate-private on purpose: the driver's handle is its only door.
pub mod observation_intake;
pub mod outbox;
/// The road in the runtime's pass — the succession statements an account is
/// owed at, delivered in every full pass of a seed-holding runtime
/// (`identity-succession.md` § Enforcement on the home nest → *Every nest the
/// identity is linked to*, **The road**).
pub mod owed_delivery;
pub mod p2p_participation;
/// The page loop every plane walk runs.
pub mod page_walk;
/// The scheduler yield a pass takes per unit of local work.
pub mod pass_breath;
pub mod peer_anchor_rows;
/// The preference cluster's plane write — the local half of a preference
/// save, and the kinds it admits (`config-dissolution.md` § The `__config`
/// dissolution schedule, the E1 cluster).
pub mod preference_put;
/// The preference surfaces over the account store — one implementation for
/// every host, web's wasm surfaces included (`config-dissolution.md` § The
/// `__config` dissolution schedule → *The closure order*, steps (2) and (5));
/// re-exported by `fauna_sync_engine::preference_surfaces` at its old path.
#[cfg(feature = "account-driver")]
pub mod preference_surfaces;
/// The T10 principal bundle itself — one implementation for every host,
/// generic over the secret store and the write section (ruling (4)'s decision
/// (c)); natively instantiated by `fauna_sync_engine::principal_bundle`.
#[cfg(feature = "account-driver")]
pub mod principal_bundle;
/// The credential-slot seam — what the driver reads and writes on the
/// machine's principal bundle, as a trait the native slot and web's implement.
pub mod principal_custody;
/// Principal succession's two assembly steps — the ceremony-time probe (and
/// the rotation it licenses) and the lost-slot heal — generic over the secret
/// store, the slot section and the store backend; every host's assembly runs
/// them (natively re-exported by `fauna_sync_engine::principal_succession`).
#[cfg(feature = "account-driver")]
pub mod principal_succession;
/// The bind leg's row half: after a reconcile, push verbatim what the bound
/// nest's listing lacks (`account-sync-plane.md` § The bind leg, ruling 1).
pub mod publish_diff;
pub mod refused_change_rows;
/// The removal's nest half: at every nest a runtime completes, the grant of
/// every roster row whose principal merged state reads removed is revoked by
/// key (`account-data-taxonomy.md` § Fleet-scope reclamation, clause (4) →
/// *The nest half follows merged state*).
pub mod removed_grants;
pub mod scope_set;
pub mod seen_set_producer;
pub mod subscription_rows;
pub mod succession_ledger_rows;
/// Principal succession's un-pushed-tail re-author — the store-only pump
/// step the probe's rotation and the lost-slot heal arm
/// (`principal_succession`).
pub mod succession_tail;
/// The unkeyed hold's predicate — whether a generation a listing left rows
/// unopened under may still be keyed for this device
/// (`account-client-lifecycle.md` § *The first listing*, clause (5)).
pub mod unkeyed_hold;
/// Web's host of the account driver — the `spawn_local` twin of the native
/// store thread, over the IndexedDB backend and a Web Locks election
/// (ruling (4)'s decision (f)).
#[cfg(all(target_arch = "wasm32", feature = "account-driver"))]
pub mod web_host;

/// The frontier writer-count ceiling the page loop enforces. Re-exported
/// because the nest derives the admin-settable `AdminTier::max_devices` bound
/// from it — the relationship the ceiling's own sizing comment assumes
/// (`account-sync-plane.md` § Feeds and cursors).
pub use page_walk::MAX_FRONTIER_WRITERS;
