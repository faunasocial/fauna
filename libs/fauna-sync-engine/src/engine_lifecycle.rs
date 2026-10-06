//! Build **one** folder engine from nest state: the client-side construction
//! every engine host shares.
//!
//! Three shared modules, three jobs — keep them straight:
//!
//! - [`engine_host`](crate::engine_host) — *multiplexing*: N engines on one worker
//!   thread, live start/stop, the generation guard.
//! - [`always_resident`](crate::always_resident) — *driving* one engine that is
//!   already built: the initial convergence pass, the owner-only re-seal, and the
//!   watch/debounce/rescan loop (the per-user `fauna-sync-agent` drives it).
//! - **this module** — *building* one engine, **the one builder**
//!   (`on-demand-files.md` § Shared sets on a capability host → *One
//!   mechanism*, question 2): the in-process hosts in `fauna-ffi` (the
//!   apple/android `FfiSyncEngineHost`'s construct-run-drop library ingress, the
//!   File Provider host) and every engine the desktop sync agent runs.
//!
//! This module owns what happens when one of those engines is built, and it is
//! the same on every host:
//!
//! 1. register the device (`fauna.sync.register`) — unless the host's app does,
//! 2. read the set's row — a same-nest set's in the nest-authoritative folder
//!    roster, a cross-nest set's in its holder's custody record (it has no row
//!    here; custody is the row),
//! 3. resolve that binding **fail-closed** ([`decide_engine_content_binding`]),
//! 4. assemble the [`SyncEngine`](crate::engine::SyncEngine)
//!    ([`assemble_engine`]) — for a cross-nest set, its byte plane at the home
//!    nest and its control plane relayed through the own nest.
//!
//! An engine's keys are final at build. A host that keeps one **resident** — the
//! control-inverted on-demand host, which has no app to rebuild it — re-resolves
//! at its own edges: it reads the set's row ([`fetch_binding_row`]), compares it
//! with what the engine was built on ([`edge_verdict`]), and rebuilds over the
//! same state DB when the binding, the caller's access or the content-key floor
//! moved, or the floor is still ahead of the generation it holds
//! (`docs/goal/behavior/on-demand-files.md` § Shared sets on a capability host,
//! decision 2).
//!
//! The in-process **resident** drive (a watcher-driven engine per device-local
//! binding, plus the iOS one-shot pass over those bindings) was retired
//! 2026-09-25: no app binds a location in-process any more — every agent-backed
//! app binds through the agent, and iOS has no location-binding surface
//! (`docs/goal/behavior/on-demand-files.md` § Hosting multiple on-demand
//! folders).
//!
//! **Why this is shared and not per-app.** Step 3 is a security boundary: a
//! *bound* (cross-user shared) set must never seal its content in plaintext, so
//! an indeterminate binding refuses to run at all. A second copy of a
//! fail-closed rule is a second thing to get wrong, so every in-process host
//! builds through **one** implementation (priority #2/#4).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use fauna_client::{AuthClient, NestClient, graduate_home_nest_pin, pinned_http_client};
use fauna_client_folders::FolderKeyReader;
use fauna_client_folders::folders::FolderSummary;
use fauna_client_folders::{
    resolve_engine_key_binding, resolve_foreign_engine_key_bindings, retired_serve_custody,
};
use fauna_core::crypto::BackupKey;
use fauna_core::data::FoldersConfig;
use fauna_core::folder_keys::{FolderContentKeys, FolderRef};
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;
use fauna_nest_http::BearerSource;
use fauna_protocol::folders::AttestationMemory;

use crate::access_gate::AccessGate;
use crate::adaptive::AdaptiveConcurrency;
use crate::binding_edge::SealFloor;
use crate::db::SyncDb;
use crate::engine::{SyncEngine, default_format_registry};
use crate::ignore::IgnoreMatcher;
use crate::nest_client::SyncClient;
use crate::progress::ProgressTx;
use crate::transfer::TransferPool;

// ---------------------------------------------------------------------------
// Transfer tuning — hard-coded constants (no config file).
// ---------------------------------------------------------------------------

const PARALLEL_DOWNLOADS: usize = 4;
const MAX_CONCURRENT_CHUNKS: u32 = 8;

/// Attempts + delay for the two post-login reads a bound-set engine needs before
/// it can be built (the folder list + the owner's folder-keys custody). An engine
/// can start moments after login, before the WS-RPC socket is ready, so both reads
/// retry briefly rather than failing the binding on a warm-up race — the same
/// best-effort pattern the backup coordinator uses.
const BINDING_LOAD_ATTEMPTS: usize = 10;
const BINDING_LOAD_RETRY_DELAY: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// Device-local sync identity
// ---------------------------------------------------------------------------

/// Read (or create + persist) this device's stable sync device id.
///
/// The id must be the **same** across all of this device's per-folder engines
/// (so the nest sees one device, not N), so it lives in a dedicated `device.db`
/// rather than in any single folder's state DB.
pub fn load_device_id(state_dir: &std::path::Path) -> Result<[u8; 32]> {
    std::fs::create_dir_all(state_dir)?;
    let db = SyncDb::open(state_dir.join("device.db"))?;
    db.get_or_create_device_id()
}

/// The install device secret's file name, under an app's **install-scoped**
/// sync dir — never inside an actor scope, and named by no sign-out sweep, so
/// it survives the erase that takes every `device.db`.
pub const INSTALL_DEVICE_SECRET_FILE: &str = "install-device-secret";

/// Read (or mint + persist) this install's device secret under `install_dir`.
///
/// App and co-located agent may both arrive first, so the mint is a
/// `create_new`: the loser of that race reads the winner's bytes instead of
/// overwriting them — two processes deriving from two secrets would register
/// two rows.
pub fn load_or_create_install_device_secret(
    install_dir: &std::path::Path,
) -> Result<[u8; fauna_core::device_id::INSTALL_DEVICE_SECRET_LEN]> {
    use std::io::Write;

    std::fs::create_dir_all(install_dir)?;
    let path = install_dir.join(INSTALL_DEVICE_SECRET_FILE);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            let secret = fauna_core::device_id::mint_install_device_secret();
            file.write_all(&secret)?;
            file.sync_all()?;
            Ok(secret)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => std::fs::read(&path)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("the install device secret is not 32 bytes")),
        Err(e) => Err(e.into()),
    }
}

/// Read (or derive + persist) the sync device id `actor_id` registers under
/// on this install — the production get-or-create every app's sign-in reaches.
///
/// `actor_state_dir` is the account-scoped dir a sign-out erases; `install_dir`
/// is the install-scoped one it does not. A persisted id always wins (a
/// row already persisted keeps its id; the e2e session door's
/// [`store_device_id_hex`] keeps working), and an empty store derives
/// [`fauna_core::device_id::derive_device_id`] instead of minting at random —
/// so the sign-in after a sign-out comes back to its own `sync_devices` row.
/// Owner: `sync-agent-credentials.md` § Credential model, the 2026-09-20 ruling.
pub fn load_device_id_for_actor(
    install_dir: &std::path::Path,
    actor_state_dir: &std::path::Path,
    actor_id: &[u8; 32],
) -> Result<[u8; 32]> {
    std::fs::create_dir_all(actor_state_dir)?;
    let db = SyncDb::open(actor_state_dir.join("device.db"))?;
    if let Some(id) = db.get_device_id()? {
        return Ok(id);
    }
    let secret = load_or_create_install_device_secret(install_dir)?;
    let id = fauna_core::device_id::derive_device_id(&secret, actor_id);
    db.set_device_id(&id)?;
    Ok(id)
}

/// Adopt `hex` as this device's stable sync device id, writing it into the
/// same `device.db` [`load_device_id`] reads. The three failure modes
/// (malformed hex, unopenable store, write failure) collapse into one error
/// whose `Display` names which one — the caller supplies its own logging
/// prefix/context, matching [`load_device_id`]'s own "no logging inside the
/// shared fn" shape. Linux and tui each hand-rolled this exact
/// decode-open-write sequence (linux's `sync::adopt_device_id_hex`, tui's
/// `media::adopt_device_id_hex`) before it was lifted here (priority #2).
pub fn store_device_id_hex(state_dir: &std::path::Path, hex: &str) -> Result<()> {
    let id = fauna_core::hex32::decode(hex)
        .map_err(|e| anyhow::anyhow!("refusing a device id that is not 32-byte hex: {e}"))?;
    let db = SyncDb::open(state_dir.join("device.db"))
        .map_err(|e| anyhow::anyhow!("could not open the device-id store: {e}"))?;
    db.set_device_id(&id)
        .map_err(|e| anyhow::anyhow!("could not store the device id: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// How one engine gets its **owner content-seal root** — the identity seed, or a
/// pre-derived owner `BackupKey`.
///
/// The identity-holding clients (Linux, the apple `FfiSyncEngineHost`) supply the
/// raw Ed25519 seed; the app-dead, capability-scoped apple File Provider host
/// supplies a pre-derived owner `BackupKey` and **no seed** — the least-privilege
/// delegate that never holds the identity seed (`file-sync.md` § Who runs the
/// hydration host, rule #7; the same shape as the bearer-only Windows service,
/// which likewise builds its engine from a provisioned `BackupKey`).
#[derive(Clone)]
pub enum EngineCredential {
    /// The owner's raw Ed25519 secret (hex). Derives the owner `BackupKey` (the
    /// owner-only seal root) **and** the change signer. Empty/undecodable ⇒ owner-only uploads
    /// fail closed rather than upload plaintext.
    Seed(String),
    /// A pre-derived owner `BackupKey`, held by a seed-less capability host (the
    /// apple File Provider extension, android's provider). Decrypts owner-only
    /// content directly; a bound or served set's content keys come from
    /// [`EngineParams::folder_keys`] instead — the `BackupKey` alone suffices
    /// (`on-demand-files.md` § Shared sets on a capability host, decision 1).
    BackupKey(BackupKey),
}

/// The owner content-seal root for a credential: derive it from the identity seed
/// (the seed-holding clients) or take the pre-derived key (the seed-less File
/// Provider host).
///
/// `None` only for a `Seed` whose hex won't decode — owner-only uploads then fail
/// closed rather than seal plaintext. The two forms are **equivalent by
/// construction**: `BackupKey(BackupKey::derive(&seed))` is the identical key
/// `Seed(hex(seed))` derives (pinned by the equivalence test below), so a seed-less
/// host seals owner-only content under exactly the root the identity-holding app
/// would.
fn owner_backup_key(credential: &EngineCredential) -> Option<BackupKey> {
    match credential {
        EngineCredential::Seed(secret_hex) => fauna_core::hex32::decode(secret_hex)
            .ok()
            .map(|secret| BackupKey::derive(&secret)),
        EngineCredential::BackupKey(key) => Some(key.clone()),
    }
}

/// Everything one engine needs to exist, independent of platform. The caller's
/// [`EngineSpec`](crate::engine_host::EngineSpec) builds this per descriptor.
pub struct EngineParams {
    /// Where the per-set state DBs live (`fsid-<ref>.db`,
    /// [`FolderRef::state_db_path`]).
    pub state_dir: PathBuf,
    /// The local directory this engine watches + materializes into.
    pub watch_dir: PathBuf,
    /// The set this engine syncs, by its identity — the binding's only key
    /// (`on-demand-files.md` § Hosting multiple on-demand folders). Its name is
    /// a label two sets can share, so the build takes it from the set's own
    /// row ([`summary_for_ref`]) rather than from the caller.
    pub folder_ref: FolderRef,
    /// This device's stable sync device id — the **same** across all of this
    /// device's engines, so the nest sees one device rather than N.
    pub device_id: [u8; 32],
    /// The label this build registers the device under (`fauna.sync.register`),
    /// shown in the nest's device list. `None` = the build registers nothing:
    /// the desktop sync agent shares its app's device id, and the app registers
    /// that device under the user's own label — an agent registering too would
    /// overwrite it.
    pub device_label: Option<String>,
    /// Self-authenticating HTTP client for the chunk transport.
    pub auth: Arc<AuthClient>,
    /// The app's live WS-RPC control-plane connection.
    pub nest_rpc: Arc<NestClient>,
    /// The actor's shared per-actor MLS engine (the conversations rail's), needed
    /// by a **bound** (cross-user shared) set. `None` is safe: a bound set then
    /// fails closed rather than sealing in plaintext.
    pub mls: Option<Arc<MlsEngine>>,
    /// How this engine gets its owner content-seal root — the identity seed (the
    /// seed-holding clients) or a pre-derived owner `BackupKey` (the seed-less File
    /// Provider host). See [`EngineCredential`].
    pub credential: EngineCredential,
    /// Completed-file / chunk progress sink (Linux fires desktop notifications;
    /// apple drives per-file display state). `None` to discard.
    pub progress_tx: ProgressTx,
    /// The account's retired owner keys after an identity succession — **read**
    /// candidates only, never a seal root (`sync-agent.md` § Credential model →
    /// *Retired owner keys after an identity succession*). Empty for every
    /// identity that never succeeded. Each key paired with the identity it
    /// belongs to where the host knows it, nearest hop first — only a paired
    /// key is offered to a row signed as a predecessor
    /// ([`fauna_core::file_download::PredecessorSealKey`], ruling (8)(c)).
    pub predecessor_backup_keys: Vec<fauna_core::file_download::PredecessorSealKey>,
    /// The account's **attested** predecessor ids, nearest hop first
    /// (`AccountRegistry::attested_predecessor_actor_ids`, carried to the sync
    /// agent on `SyncCapability::predecessor_actor_ids`) — the reader
    /// binding's own-account source (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (8)(b), source (ii)): a row
    /// signed as one of them verifies as this account's. Public material,
    /// pushed whatever the keys' drop license says. The ids of the paired
    /// keys above are added to it, so a host handed only pairs still binds.
    pub predecessor_actor_ids: Vec<[u8; 32]>,
    /// The host's statement-walk memory — what an engine handed no attested
    /// ids (or missing one) proved its own over the succession lookup (ruling
    /// (8)(b), source (ii); `SyncEngine::prove_own_links`). One per host,
    /// cloned into every engine it builds: a link proven by one build seeds
    /// the next build's binding, so it is not asked for again and does not
    /// read as a fresh gain owing another head re-judge. `Default` for a host
    /// that builds each engine once.
    pub learned_predecessors: fauna_client_sync::row_judge::LearnedPredecessors,
    /// The terminal `access-revoked` park flag (`file-sync.md` § Multi-writer
    /// shared sets, D4), shared by the engine and a cross-nest set's byte-plane
    /// bearer so a record refusal and a mint refusal land in one state. `Some`
    /// when the host observes the park (the sync agent persists it); `None` =
    /// a fresh gate nobody outside the engine watches.
    pub access_gate: Option<Arc<AccessGate>>,
    /// The machine's change-record signer
    /// ([`crate::principal_bundle::load_change_signer`] — the principal writer
    /// key + its `SyncWrite` grant), resolved by the caller so this library
    /// never reaches the OS credential store itself. `None` on a seed-holding
    /// credential signs directly with the identity key (ruling (1): a host
    /// with no principal yet); `None` on a seed-less one records unsigned.
    pub change_signer: Option<Arc<fauna_protocol::sync_writer_sig::ChangeSigner>>,
    /// The account's folder-key custody (`fauna.state.folder-keys`) — a bound,
    /// served or cross-nest set's keys and every set's nonce. The seat's store
    /// on a runtime-hosting process, the capability host's cold fleet replica
    /// on one that hosts none (`on-demand-files.md` § Shared sets on a
    /// capability host, decision 1′). Read-only by type: an engine host never
    /// writes custody.
    pub folder_keys: Arc<dyn FolderKeyReader>,
    /// What this host does with a set the account may only read
    /// ([`ReaderHosting`]).
    pub reader_hosting: ReaderHosting,
}

/// What a host does with a set shared *with* the account that the account may
/// only read (`FolderSummary::is_reader_member`; a cross-nest record without a
/// `writer` grant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReaderHosting {
    /// Refuse to build: the host sits over a directory the user can write
    /// behind the engine's back — a bound folder, a watched ingress — where an
    /// edit that could not upload would breach `file-sync.md`'s iron rule that
    /// a tracked file's local modification MUST be uploaded. Every host but a
    /// control-inverted one, and the default.
    #[default]
    Refuse,
    /// Build the set **read-only**: a control-inverted host (apple's File
    /// Provider extension, android's documents provider) has no such
    /// directory — the OS or the provider owns the tree and only ever *calls*
    /// — so a reader's engine pulls, lists and opens, and the provider face
    /// refuses every write on it (`on-demand-files.md` § Shared sets on a
    /// capability host, decision 3).
    ReadOnly,
}

/// The signer a build's records carry: the caller's, else — on a seed-holding
/// credential — the identity key itself. `None` records unsigned.
fn effective_change_signer(
    change_signer: Option<Arc<fauna_protocol::sync_writer_sig::ChangeSigner>>,
    credential: &EngineCredential,
) -> Option<Arc<fauna_protocol::sync_writer_sig::ChangeSigner>> {
    change_signer.or_else(|| match credential {
        EngineCredential::Seed(secret_hex) => {
            fauna_core::hex32::decode(secret_hex).ok().map(|secret| {
                Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
                    &ActorKeypair::from_secret(secret),
                ))
            })
        }
        EngineCredential::BackupKey(_) => None,
    })
}

/// A built engine, ready for its caller's construct-run-drop use (the
/// library-ingress upload, the File Provider host's served requests).
pub struct BuiltEngine {
    pub engine: SyncEngine,
    /// The row facts the key binding was resolved from — what a resident host
    /// compares at its re-resolve edges ([`edge_verdict`]).
    pub basis: BindingBasis,
    /// The content-key generation the engine seals under: `Some` for a bound or
    /// served set whose keys custody supplied, `None` for an owner-only set and
    /// for a bound set built keyless (which fails closed per operation).
    pub held_version: Option<u64>,
    /// The engine is a reader's — built read-only under
    /// [`ReaderHosting::ReadOnly`]. A host skips its seal edge, its sweep and
    /// its re-seal drive for it: nothing it does seals.
    pub read_only: bool,
}

// ---------------------------------------------------------------------------
// Re-resolve edges — the comparison lives in the always-compiled
// `crate::binding_edge` (the pre-seal hold is the engine's own); re-exported here
// for the hosts that build through this module.
// ---------------------------------------------------------------------------

pub use crate::binding_edge::{
    BindingBasis, EdgeVerdict, SealHold, edge_verdict, floor_ahead, summary_for_ref,
};

impl BuiltEngine {
    /// [`edge_verdict`] for this engine.
    #[must_use]
    pub fn edge_verdict(&self, now: Option<&BindingBasis>) -> EdgeVerdict {
        edge_verdict(&self.basis, self.held_version, now)
    }

    /// Why a seal now would be held — the engine's own pre-seal hold
    /// ([`SyncEngine::seal_hold`]), armed at build from the row's floor. `Some`
    /// means a seal would be under a generation the owner has rotated past, so the
    /// write must be held.
    #[must_use]
    pub fn seal_hold(&self) -> Option<SealHold> {
        self.engine.seal_hold()
    }

    /// Why a write would be held back from the nest — the engine's publication
    /// hold ([`SyncEngine::publish_hold`]): every seal hold, plus a floor the
    /// last read could not refresh. A host that acknowledges a write only once
    /// it is recorded asks this one (decision 2′).
    #[must_use]
    pub fn publish_hold(&self) -> Option<SealHold> {
        self.engine.publish_hold()
    }
}

/// Read the set's binding basis now, by identity — one attempt, no warm-up
/// retry: an edge runs on a host that is already serving. `None` = the read
/// failed; `Some(None)` = a successful read without the set.
///
/// A same-nest set's basis is its row in this nest's list. A cross-nest set's
/// is its holder's custody record (it has no row here — custody is the row),
/// so its edge is a custody read under the same credential the build used:
/// the only way a rotation reaches a resident foreign engine.
pub async fn fetch_binding_basis(
    nest_rpc: &Arc<NestClient>,
    folder_keys: &dyn FolderKeyReader,
    folder_ref: FolderRef,
) -> Option<Option<BindingBasis>> {
    match folder_ref {
        FolderRef::Local(_) => match fauna_client_folders::FoldersClient::new(nest_rpc.clone())
            .list_owned_and_shared_wire()
            .await
        {
            Ok(reply) => Some(summary_for_ref(&reply.folders, folder_ref).map(BindingBasis::of)),
            Err(e) => {
                tracing::warn!("folder list for an engine's re-resolve edge: {e}");
                None
            }
        },
        FolderRef::Foreign(channel) => match folder_keys.load().await {
            Ok(cfg) => Some(foreign_binding_basis(&cfg, channel)),
            Err(e) => {
                tracing::warn!("custody read for a cross-nest engine's re-resolve edge: {e:#}");
                None
            }
        },
    }
}

/// A cross-nest set's basis from the holder's custody `cfg`: its record plus
/// the newest generation custody holds for it. `None` = no record (left or
/// removed).
fn foreign_binding_basis(cfg: &FoldersConfig, channel: [u8; 32]) -> Option<BindingBasis> {
    let record = fauna_client_folders::custody::find_foreign_set(cfg, &channel)?;
    let generation =
        fauna_client_folders::custody::content_keys(cfg, &channel).map(|k| k.current_version());
    Some(BindingBasis::of_foreign(record, generation))
}

// ---------------------------------------------------------------------------
// Step 3 — the fail-closed content-key binding decision (security boundary)
// ---------------------------------------------------------------------------

/// The **`public`-audience write-arm verdict** for the engine being built
/// (phase 4), and the replay memory the seat must persist with it.
///
/// `unsealed: true` ⇒ the owner's genuine attestation over this row verified
/// under the seat's trusted owner at or above its floor
/// (`FolderSummary::judge_declassification` — `encryption-at-rest.md`
/// § Readable classes → *The declassification is owner-ATTESTED*), so uploads
/// rest unsealed by ratified design. Deliberately *additive* to the key
/// material: a declassified **bound** folder keeps its group id and content
/// keys (they still open the sealed pre-declassify back-catalogue); only the
/// write side goes plaintext. `memory` is what the verifier handed back — the
/// caller persists it (`SyncDb::set_audience_attestation_memory`) whatever the
/// verdict, because the sealed one is the one that burns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclassificationVerdict {
    pub unsealed: bool,
    pub memory: AttestationMemory,
}

/// The fail-closed decision for one engine's content-key binding, factored out of
/// the async orchestration so the security-critical branches are unit-testable.
#[derive(Debug, PartialEq, Eq)]
pub enum EngineContentDecision {
    /// Build the engine with these `(mls_group_id, content_keys)` args (the
    /// pair goes straight into `SyncEngine::new`) and this declassification
    /// verdict (its flag via `with_public_audience`, its memory persisted).
    Run(
        Option<Vec<u8>>,
        Option<FolderContentKeys>,
        DeclassificationVerdict,
    ),
    /// The binding is *indeterminate* (the folder list was unavailable, or a
    /// bound set's `mls_group_id` projection is corrupt) → refuse to run rather
    /// than risk sealing a shared set in plaintext. Nothing is judged, so the
    /// seat's memory is untouched.
    Refuse,
}

/// Decide `(mls_group_id, content_keys)` for one folder, FAIL CLOSED.
///
/// - `list_ok` — did `fauna.folders.list` succeed? A failed list makes the
///   binding *indeterminate* (owner-only vs bound is unknowable) → [`Refuse`].
/// - `summary` — this set's row from a *successful* list (`None` = absent =
///   deleted / no longer shared → owner-only, safe to run unbound).
/// - `config` — the owner's folder-key custody, loaded only for a *bound* or
///   *WebDAV-served* set; `None` when owner-only (never loaded) or when the
///   custody load failed (→ bound-keyless fail-closed, or served → refuse).
/// - `trusted_owner` — the identity the row's audience attestation must be
///   signed by (`fauna_client_folders::DeclassificationAnchor::trusted_owner_for`:
///   the seat's own actor id for an owned row, the channel's MLS-recorded owner
///   for a member row; `None` seals) — and `memory`, what this seat remembers
///   about the folder's attestations (`SyncDb::audience_attestation_memory`).
///   The verdict is judged over the row's own name, the name an id-bound seat
///   acts under.
///
/// This extends the resolver's fail-closed contract over the two network-failure
/// edges: **a bound set is never mapped to `(None, None)`** — it stays
/// `Some(mls_group_id)` (keyless when custody is unavailable) so the engine fails
/// closed; **a served group-less set with no resolvable keys never runs at all**
/// (there is no in-engine fail-closed marker without a group id, and the owner path
/// would seal under `BackupKey`/plaintext — FS-BIND-5/FS-5DC); and an undetermined
/// binding refuses outright.
///
/// [`Refuse`]: EngineContentDecision::Refuse
pub fn decide_engine_content_binding(
    list_ok: bool,
    summary: Option<&FolderSummary>,
    config: Option<&FoldersConfig>,
    trusted_owner: Option<&fauna_core::identity::ActorId>,
    memory: AttestationMemory,
    reader_hosting: ReaderHosting,
) -> EngineContentDecision {
    // List unavailable → binding indeterminate → fail closed.
    if !list_ok {
        return EngineContentDecision::Refuse;
    }
    // The `public`-audience write-arm verdict (phase 4), additive to the key
    // resolution below: uploads rest unsealed iff the OWNER's attestation on
    // the row verifies under this seat's trusted owner and is not a replay —
    // never because the nest's projection says `public`.
    // The rule, the nest-refused `webdav_enabled` + `public` fail-safe
    // included, is `FolderSummary::judge_declassification`'s; a row absent
    // from a successful list seals and burns.
    let (unsealed, memory) = FolderSummary::judge_listed_declassification(
        summary,
        summary.map_or("", |s| s.name.as_str()),
        trusted_owner,
        memory,
    );
    let public_audience = DeclassificationVerdict { unsealed, memory };
    // Absent from a *successful* list → deleted / no longer shared with the owner.
    // Not a bound set (no row), so owner-only semantics are safe.
    let Some(summary) = summary else {
        return EngineContentDecision::Run(None, None, public_audience);
    };
    // Reader-unbindable guard (multi-writer Phase 1, `file-sync.md` § Multi-writer
    // shared sets): a set shared *with* the caller (`role == "member"`) is bindable
    // only by a WRITER. A reader must never run a sync engine — there is no
    // read-only mirror in v1, and a bound folder whose edits could not upload would
    // breach `file-sync.md`'s iron rule that a tracked file's local modification
    // MUST be uploaded. Refuse rather than half-sync; fail closed on absent access
    // (⇒ reader). Owner rows (`role == "owner"`, or an unresolved row whose `role` is absent) never
    // trip this — the reader/writer axis is meaningful only for member rows. This
    // also covers a writer→reader demotion of an already-bound set: its stale
    // location-map entry now refuses to run rather than silently failing every upload.
    //
    // The one host the rule does not reach is a control-inverted one
    // (`ReaderHosting::ReadOnly`): it has no directory the user could edit, so
    // a reader's set resolves like a writer's below and the caller builds the
    // engine read-only.
    if summary.is_reader_member() && reader_hosting == ReaderHosting::Refuse {
        return EngineContentDecision::Refuse;
    }
    // Owner-only (no group binding, not WebDAV-served) → owner-`BackupKey` path.
    // Served is the owner's word in custody, never the roster's flag
    // (`writer-signed-change-records.md` ruling (7)(b)(ii) rule (2)). With
    // custody unreadable the flag is consulted only to REFUSE below — a read
    // that can only withhold an engine, never content-key or exempt one.
    let served = config.map_or(summary.webdav_enabled, |cfg| {
        fauna_client_folders::custody_served(summary, cfg)
    });
    if summary.mls_group_id.is_none() && !served {
        return EngineContentDecision::Run(None, None, public_audience);
    }
    // Bound (cross-user shared) or WebDAV-served set. Resolve content keys from
    // custody when the config loaded; otherwise fail closed — bound-keyless for
    // a shared set (the engine refuses per-op), refuse-outright for a served
    // group-less set (no engine representation).
    match config {
        Some(cfg) => match resolve_engine_key_binding(cfg, summary) {
            Ok(binding) => match binding.engine_args() {
                Some((gid, keys)) => EngineContentDecision::Run(gid, keys, public_audience),
                // Served-but-keyless: no engine may run (see doc above).
                None => EngineContentDecision::Refuse,
            },
            // Corrupt `mls_group_id` projection — bound but unusable. Refuse rather
            // than downgrade to unbound (the resolver's own contract).
            Err(_) => EngineContentDecision::Refuse,
        },
        None => match summary.mls_group_id.as_deref() {
            Some(hex_gid) => match hex::decode(hex_gid) {
                // Bound-keyless: known bound (Some raw gid), custody unavailable.
                Ok(raw) => EngineContentDecision::Run(Some(raw), None, public_audience),
                // Corrupt hex → cannot even build the bound-marker → refuse.
                Err(_) => EngineContentDecision::Refuse,
            },
            // Served group-less set whose custody could not be loaded → refuse.
            None => EngineContentDecision::Refuse,
        },
    }
}

// ---------------------------------------------------------------------------
// Steps 1–4 — build one engine
// ---------------------------------------------------------------------------

/// Fetch this device's folder summaries for engine binding, retrying through
/// the WS-RPC warm-up. `None` after the final attempt — the caller treats an
/// unavailable list as an *indeterminate* binding and fails closed.
///
/// Uses the `include_shared_with_me` projection (`list_owned_and_shared`), not the
/// owner-only `list`: a **writer member** binds a set shared *with* this device, so
/// that set's `role == "member"` summary (carrying `mls_group_id` + `access`) must
/// appear here for `decide_engine_content_binding` to resolve its content key from
/// the member's own custody (multi-writer Phase 1). Owner rows are unaffected (they
/// appear in both projections); a rostered-but-unbound set never builds an engine
/// (only a location-map entry does), and a reader member is refused by the guard.
async fn fetch_folders_retry(nest_rpc: &Arc<NestClient>) -> Option<Vec<FolderSummary>> {
    let client = fauna_client_folders::FoldersClient::new(nest_rpc.clone());
    match fauna_sleep::retry(BINDING_LOAD_ATTEMPTS, BINDING_LOAD_RETRY_DELAY, || {
        client.list_owned_and_shared_wire()
    })
    .await
    {
        Ok(reply) => Some(reply.folders),
        Err(e) => {
            tracing::warn!("folder list for engine binding: {e}");
            None
        }
    }
}

/// Read the holder's folder-key custody (`fauna.state.folder-keys`) through
/// the host's [`FolderKeyReader`], retrying through the warm-up — the M2
/// content-key custody a *bound* set's engine reads. `None` when the read
/// ultimately fails; the caller then fails closed (bound-keyless, never
/// plaintext).
async fn load_custody_retry(folder_keys: &dyn FolderKeyReader) -> Option<FoldersConfig> {
    match fauna_sleep::retry(BINDING_LOAD_ATTEMPTS, BINDING_LOAD_RETRY_DELAY, || {
        folder_keys.load()
    })
    .await
    {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            tracing::warn!("folder engine: folder-key custody unreadable: {e:#}");
            None
        }
    }
}

/// A set's resolved binding — everything [`assemble_engine`] needs beyond the
/// host's own [`EngineParams`]: what the fail-closed decision settled (the group
/// marker, the content keys, the audience), the row facts the engine is armed
/// from, and — for a cross-nest set — where its planes dial.
///
/// [`build_engine`] is the only production producer: it resolves this from the
/// nest (a same-nest set's row) or from custody (a cross-nest set's record), and
/// a resolution that must not run never yields one. The derived `Default` is the
/// owner-only, same-nest shape — what a test assembling an engine by hand starts
/// from.
#[derive(Clone, Default)]
pub struct ResolvedBinding {
    /// The set's name — a label (logs, the engine's `changes.list` scope), never
    /// a key.
    pub folder: String,
    /// The raw MLS group id — the engine's bound-marker. `Some` with `None` keys
    /// fails every operation closed.
    pub mls_group_id: Option<Vec<u8>>,
    /// The generation history custody supplied, for a bound or served set.
    pub content_keys: Option<FolderContentKeys>,
    /// The `public`-audience write arm (phase 4): uploads rest unsealed.
    pub public_audience: bool,
    /// A once-served set's retired generation — a read candidate for the
    /// re-seal walk, never a seal root (`webdav-server.md` § Key model,
    /// Revocation).
    pub retired_content_keys: Option<FolderContentKeys>,
    /// Rule (5)'s re-seal hold (`writer-signed-change-records.md` ruling
    /// (7)(b)(ii)): the roster flags this owned set served while its custody is
    /// keyed and carries no serve stamp — a set served before the stamps
    /// existed, until the owner's one flip ON. The re-seal pass waits rather
    /// than walk the back-catalogue onto the owner root that flip would walk
    /// straight back ([`served_era_hold`]).
    pub served_era_hold: bool,
    /// The set's conflict policy (`file-sync.md` § Conflicts).
    pub conflict_policy: fauna_core::format::ConflictPolicy,
    /// Phase 5 metadata-only residency (`file-sync.md` § Content residency):
    /// `Some(true)` metadata-only, `Some(false)` full, `None` unknown — a
    /// cross-nest set whose custody record no home nest has stamped. Unknown
    /// arms nothing and persists no reading: the seat uploads, and the
    /// holder-keeps gate keeps every own-record body (`file-sync.md` § Relay
    /// serving → *A member on another nest*, step (1)).
    pub metadata_only_residency: Option<bool>,
    /// Decision 2's pre-seal hold, armed from the basis.
    pub seal_floor: SealFloor,
    /// What the binding was resolved from — what a resident host's edges compare.
    pub basis: BindingBasis,
    /// `Some` for a cross-nest set: its byte plane dials the home nest.
    pub foreign: Option<ForeignTransport>,
    /// The set's live nonce from custody — the binding every record a signing
    /// engine writes covers (`mls-group-key-material.md` § M2 → *Writer-signed
    /// change records*). `None` records unsigned.
    pub set_nonce: Option<[u8; 32]>,
    /// The other nonces this set held while it lived (ruling (g) — a race
    /// loser's, a duplicate the reconcile retired): the engine re-signs this
    /// device's heads recorded under one onto [`Self::set_nonce`]
    /// ([`SyncEngine::rerecord_under_live_nonce`]).
    pub retired_set_nonces: Vec<[u8; 32]>,
    /// The identity that minted [`Self::set_nonce`] and the set's lineage of
    /// retired nonces with their minters (`writer-signed-change-records.md`
    /// ruling (11)(b)) — what the reader judges a predecessor's row under:
    /// refused, history or current.
    pub set_nonce_minted_by: Option<[u8; 32]>,
    pub retired_lineage: Vec<([u8; 32], Option<[u8; 32]>)>,
    /// This device's adoption marker for the set (ruling (11)(d)) — the nonce
    /// its own re-mint replaced; the take-over's licence to adopt the nest's
    /// history heads once.
    pub adoption_marker: Option<[u8; 32]>,
    /// The set's owner, for the reader's writer check (ruling (3) — the owner
    /// is always a writer): this account for an owned set, the channel's
    /// MLS-recorded owner for a member's — never the list row's
    /// `owner_actor_id`. `None` on a same-nest binding that is not
    /// [`Self::member`]'s means this account (the owner-only default a
    /// hand-built test starts from); on a member's or a cross-nest one it
    /// means the host holds no marker, and the reader takes the owner off the
    /// roster's owner row (`writer-signed-change-records.md` ruling (11)(c)).
    pub owner_actor_id: Option<[u8; 32]>,
    /// The binding was resolved from a same-nest **member** row: this account
    /// does not own the set, so an absent [`Self::owner_actor_id`] is no
    /// owner, never this account.
    pub member: bool,
    /// The set is WebDAV-served: its pseudo-device rows are exempt from the
    /// reader's signature check (ruling (1)).
    pub webdav_served: bool,
    /// The account may only read the set and the host builds such a set
    /// read-only ([`ReaderHosting::ReadOnly`]).
    pub read_only: bool,
}

/// Where a cross-nest set's planes go (`federation.md` § Cross-nest shared
/// folders + channel append): the byte plane dials the **home** nest under a
/// write-token bearer minted on the own nest; the control plane relays through
/// the own nest, addressed by the set's channel id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignTransport {
    pub home_nest_url: String,
    pub channel_id_hex: String,
}

/// Build one engine for `params`: register the device, resolve the set's
/// binding **fail-closed**, and assemble the [`SyncEngine`] ([`assemble_engine`]).
///
/// **The one builder** (`on-demand-files.md` § Shared sets on a capability host →
/// *One mechanism*, question 2): the in-process hosts and the desktop sync agent
/// all build through it — a same-nest set off this nest's list, a cross-nest set
/// off its holder's custody record (custody is the row).
///
/// `None` means *refused* — the binding was indeterminate (the list was
/// unavailable; for a cross-nest set, custody was), the set is gone (no row on
/// this nest; no custody record), or the binding resolved to a shape that must
/// not run — so no engine runs at all (never a plaintext fallback). The host
/// rebuilds it on its next request or edge.
pub async fn build_engine(params: EngineParams) -> Option<BuiltEngine> {
    // The set's identity, for every log line before its name is known.
    let folder_id = params.folder_ref.to_wire();
    register_device(&params).await;
    let binding = match params.folder_ref {
        FolderRef::Local(_) => resolve_local_binding(&params).await?,
        FolderRef::Foreign(channel) => resolve_foreign_binding(&params, channel).await?,
    };
    match assemble_engine(params, binding) {
        Ok(built) => Some(built),
        Err(e) => {
            // `{:#}`: anyhow's plain Display drops the source chain, and the
            // cause (a cloud-files error on a placeholder root dir, a locked
            // state DB) is always in the source.
            tracing::error!("folder {folder_id}: build engine: {e:#}");
            None
        }
    }
}

/// Best-effort device registration over the control plane
/// (`fauna.sync.register`) so the device appears in the nest's device list — a
/// no-op when the host registers nothing ([`EngineParams::device_label`]).
async fn register_device(params: &EngineParams) {
    let Some(device_label) = params.device_label.as_deref() else {
        return;
    };
    let folder_id = params.folder_ref.to_wire();
    // Seal the user-chosen device label under the **registering owner's** root —
    // never `SyncEngine::label_seal_root()`, which resolves a *folder's* M2
    // generation (this is startup, before any `SyncEngine` exists, and a device
    // belongs to the actor rather than to a set). `owner_backup_key` is the same
    // derivation the owner-only content path uses. `None` — a `Seed` whose hex
    // will not decode — registers sealless (the row rests nameless post-flip
    // until the next keyed register re-stamps it) rather than minting a silent
    // wrong-root seal.
    let label_sealed = owner_backup_key(&params.credential).and_then(|key| {
        fauna_core::label_custody::seal_device_label(
            &fauna_core::path_crypto::LabelRoot::owner_of(&key),
            &params.device_id,
            device_label,
        )
        .unwrap_or_else(|e| {
            tracing::warn!("sealing the device label failed, registering plaintext-only: {e}");
            None
        })
    });
    // A failure usually means the nest is momentarily unreachable — sync
    // proceeds and registers next cycle. The one failure that does NOT heal by
    // itself is the tier device cap (`devices.md` § Step 4): the account
    // runtime's enrollment pass meets the same refusal for the same device and
    // records it for the Devices page (`EnrollmentRefusal::DeviceLimitExceeded`),
    // so this log line only has to name the cause instead of reading as offline.
    if let Err(e) = fauna_client_sync::SyncClient::new(params.nest_rpc.clone())
        .register(hex::encode(params.device_id), device_label, label_sealed)
        .await
    {
        if fauna_client_sync::is_device_limit_exceeded(&e) {
            tracing::error!(
                "register_device ({folder_id}): the account is at its tier's device cap — \
                 remove a device under Settings → Devices or ask the admin for a bigger \
                 tier; this device's changes cannot be recorded until a slot frees ({e})"
            );
        } else {
            tracing::error!("register_device ({folder_id}): {e}");
        }
    }
}

/// Resolve a **same-nest** set's binding from its row in this nest's list —
/// `None` = refused.
async fn resolve_local_binding(params: &EngineParams) -> Option<ResolvedBinding> {
    let folder_ref = params.folder_ref;
    let folder_id = folder_ref.to_wire();

    // The db holds this seat's audience-attestation memory (the replay floor
    // the declassification verdict below reads and writes).
    let db = match SyncDb::open(folder_ref.state_db_path(&params.state_dir)) {
        Ok(db) => db,
        Err(e) => {
            tracing::error!("open state db for {folder_id}: {e}");
            return None;
        }
    };

    // Fetch this owner's folder summaries ONCE — the single read that drives
    // the content-key binding (whether this is a *bound* cross-user shared
    // set). Retried through the WS-RPC warm-up (an engine can start moments
    // after login, before the socket is ready). `None` = the list is
    // unavailable after retries → the binding is indeterminate → refuse
    // (`decide_engine_content_binding`'s `!list_ok` arm, settled here so the
    // row lookup below runs over a real list).
    let Some(summaries) = fetch_folders_retry(&params.nest_rpc).await else {
        tracing::error!(
            "folder {folder_id}: folder list unavailable — the content-key binding is \
             indeterminate; refusing to run engine (fail closed). The host rebuilds it on \
             the next request."
        );
        return None;
    };
    // The set's own row, by identity. Absent from a successful list = deleted
    // on the nest, or the caller removed from it: a host has nothing to ingest
    // into or serve for such a set — the nest refuses a `changes.record` into
    // it — so refuse rather than run owner-only against a phantom.
    let Some(summary) = summary_for_ref(&summaries, folder_ref) else {
        tracing::error!(
            "folder {folder_id}: no row on this nest (deleted, or this account was removed \
             from it); refusing to run engine"
        );
        return None;
    };
    // Resolve the content-key binding, FAIL CLOSED: a bound set must never seal
    // its content in plaintext, so the engine is built only once we can positively
    // determine the binding (`mls-group-key-material.md` § M2 content-key
    // mechanism → the engine selects `key_for(version)` and fails closed if the
    // holder lacks that generation). EVERY set pays the one folder-keys custody
    // load: the set's nonce lives in custody from the set's creation, and two
    // sides need it — the signer (every record binds to it) and the READER,
    // which holds every signed row it cannot bind to a nonce — so a signer-less
    // owner-only host reads it too. The same load carries a bound set's
    // generations and a once-served set's retired serve custody
    // (`webdav-server.md` § Key model, Revocation).
    // Custody is read through the host's reader on every credential — the
    // seat's account store, or on a capability host the throwaway fleet replica
    // keyed by the machine principal's wraps (`on-demand-files.md` § Shared sets
    // on a capability host, decision 1′). The keys are held in memory for the
    // engine's life, never written anywhere. Retired serve custody rides the
    // same load, so a seed-less build re-seals a once-served set's
    // back-catalogue as a seeded one does. A load that fails leaves `config`
    // `None`: a bound set then builds keyless and fails closed per operation,
    // never plaintext.
    let config = load_custody_retry(&*params.folder_keys).await;
    // The engine takes its set's NAME from this row, and a sealed set's row
    // rests none (`path-sealing.md` § the set-name plane): name it from the
    // custody just loaded. A blank name here once built an engine that
    // addressed the nest by the hash of the empty string — every roster, place
    // and change read answered `not_found`. A sealed row this host cannot name
    // (custody unread, or a share's name stamp not landed) is refused, and
    // rebuilt at the next edge.
    let named;
    let summary = if summary.name.is_empty() {
        let row = config.as_ref().and_then(|cfg| {
            fauna_client_folders::engine_binding::named_for_engine_host(vec![summary.clone()], cfg)
                .pop()
        });
        let Some(row) = row else {
            tracing::error!(
                "folder {folder_id}: its sealed name cannot be opened from this host's custody \
                 yet; refusing to run engine. The host rebuilds it on the next request."
            );
            return None;
        };
        named = row;
        &named
    } else {
        summary
    };
    // Redacted once and reused by every log line below (`path-sealing.md` §
    // Sealed names & paths, S7): a log line is a confidentiality boundary
    // exactly like the wire and the DB.
    let folder_r = fauna_core::log_redact::log_folder_name(&summary.name);
    // The declassification anchor: this seat's own actor id for a folder its
    // account owns, the channel's MLS-recorded owner for a member seat
    // (`EngineParams::mls` — the identity-holding hosts carry one; the
    // bearer-only agent does not, so its member seats seal). Never a field of
    // the row (`encryption-at-rest.md` § Readable classes → *The
    // declassification is owner-ATTESTED*).
    let mls_owners = params.mls.as_deref().map(crate::config::MlsChannelOwners);
    let anchor = fauna_client_folders::DeclassificationAnchor {
        own: fauna_core::identity::ActorId(params.auth.actor_id()),
        channel_owners: mls_owners
            .as_ref()
            .map(|o| o as &dyn fauna_client_folders::FolderChannelOwners),
    };
    let trusted_owner = anchor.trusted_owner_for(summary);
    let memory = AttestationMemory::from_meta(
        db.audience_attestation_memory()
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "folder {folder_r}: reading the audience-attestation memory failed ({e}); \
                     judging fail-closed"
                );
                Some(String::new())
            })
            .as_deref(),
    );
    let (mls_group_id, content_keys, verdict) = match decide_engine_content_binding(
        true,
        Some(summary),
        config.as_ref(),
        trusted_owner.as_ref(),
        memory,
        params.reader_hosting,
    ) {
        EngineContentDecision::Run(gid, keys, verdict) => (gid, keys, verdict),
        EngineContentDecision::Refuse => {
            tracing::error!(
                "folder {folder_r}: content-key binding indeterminate (list unavailable or \
                 corrupt mls_group_id projection); refusing to run engine (fail closed). The \
                 host rebuilds it on the next Start (login retry / rotation)."
            );
            return None;
        }
    };
    // Persist what the verifier remembered — armed or burned — before the
    // engine exists: an unpersisted burn is a replay window.
    if let Err(e) = db.set_audience_attestation_memory(&verdict.memory.to_meta()) {
        tracing::warn!(
            "folder {folder_r}: persisting the audience-attestation memory failed ({e}); the \
             verdict is still armed this run"
        );
    }
    let public_audience = verdict.unsealed;
    // Retired serve custody (`webdav-server.md` § Key model, Revocation) —
    // computed SEPARATELY from `decide_engine_content_binding` above, never
    // folded into its `(mls_group_id, content_keys)` pair: those two answer
    // "what does this engine seal/read as its LIVE binding", and a retired
    // generation must never widen that answer (`fauna_core::crypto::effective_owner_key`'s
    // `content_keyed` gate). `None` on every path but a summary that is
    // CURRENTLY owner-only + unserved with custody actually loaded.
    let retired_content_keys = config
        .as_ref()
        .and_then(|cfg| retired_serve_custody(cfg, summary));
    // The set's live nonce from the custody just loaded (by channel for a
    // content-keyed set, else the owner's pick by name) binds every record;
    // its lineage with each nonce's minter is what the reader judges a
    // predecessor's rows under (ruling (11)(b)).
    let owned = summary.role.as_deref() != Some("member");
    let lineage = config
        .as_ref()
        .map(|cfg| summary_lineage(summary, cfg))
        .unwrap_or_default();
    let retired: Vec<[u8; 32]> = lineage.retired.iter().map(|r| r.nonce).collect();
    // This device's adoption marker for an owned set — the one naming a nonce
    // of its lineage (ruling (11)(d)).
    let adoption_marker = if owned && config.is_some() {
        fauna_client_folders::adoption_markers_or_none(&*params.folder_keys)
            .await
            .into_iter()
            .find(|m| retired.contains(m))
    } else {
        None
    };
    let set_nonce = lineage.live;
    let set_nonce_minted_by = lineage.live_minted_by.map(|a| a.0);
    let retired_lineage: Vec<([u8; 32], Option<[u8; 32]>)> = lineage
        .retired
        .iter()
        .map(|r| (r.nonce, r.minted_by.map(|a| a.0)))
        .collect();
    // The re-record leg reads the OWNER's retired nonces only.
    let retired_set_nonces = if owned { retired } else { Vec::new() };

    Some(ResolvedBinding {
        folder: summary.name.clone(),
        mls_group_id,
        content_keys,
        public_audience,
        served_era_hold: config
            .as_ref()
            .is_some_and(|cfg| served_era_hold(summary, cfg, retired_content_keys.is_some())),
        retired_content_keys,
        // Per-set conflict policy off the authoritative nest row (file-sync.md
        // § Conflicts); an absent field or unknown value degrades to Auto.
        conflict_policy: summary
            .conflict_policy
            .as_deref()
            .map(fauna_core::format::ConflictPolicy::from_wire)
            .unwrap_or_default(),
        // Phase 5 (`file-sync.md` § Content residency): armed at build off the
        // same list read, like the audience — the control-inverted FP host runs
        // no refresh tick, so the build seed is its only install.
        metadata_only_residency: Some(summary.is_metadata_only()),
        // Decision 2's pre-seal hold, armed from the row the binding was
        // resolved from: a set whose floor is already ahead of custody holds its
        // first write.
        seal_floor: SealFloor::Floor(summary.content_key_floor),
        basis: BindingBasis::of(summary),
        foreign: None,
        set_nonce,
        retired_set_nonces,
        set_nonce_minted_by,
        retired_lineage,
        adoption_marker,
        // The anchor's answer and nothing else: this account for an owned
        // row, the channel's recorded marker for a member's — and none on a
        // member row whose host holds no marker, where the reader takes the
        // owner off the roster's owner row. Never the list row's
        // `owner_actor_id` (ruling (11)(c)).
        owner_actor_id: trusted_owner.map(|a| a.0),
        member: !owned,
        // The reader's exemption is the custody entry's serve window (ruling
        // (7)(b)(ii) rule (2)) — the owner's entry, or a member's received
        // copy at the real channel — off the custody the nonce was read from.
        // Custody unreadable exempts nothing.
        webdav_served: lineage.webdav_served(),
        // Reached for a reader only under `ReaderHosting::ReadOnly` — the
        // decision above refused it otherwise.
        read_only: summary.is_reader_member(),
    })
}

/// Rule (5)'s re-seal hold (ruling (7)(b)(ii)): an owned set the roster flags
/// served, whose custody holds retired serve generations (`retired`: keyed,
/// not served) and no serve stamp at its custody channel — what a set served
/// before the stamps existed looks like until the owner's one flip ON. A nest
/// can only DELAY a re-seal this way, the safe direction; a stamped entry the
/// owner served off is never held, whatever the flag says.
fn served_era_hold(summary: &FolderSummary, cfg: &FoldersConfig, retired: bool) -> bool {
    retired
        && summary.webdav_enabled
        && summary.role.as_deref() != Some("member")
        && fauna_client_folders::owned_custody_channel(summary).is_ok_and(|channel| {
            fauna_client_folders::custody::serve_stamps(cfg, &channel) == (None, None)
        })
}

/// A same-nest set's nonce lineage and serve window off the custody `cfg`
/// just loaded — the owner's pick by name for an owned set, else the entry at
/// the set's custody channel (a member's received copy at the real channel).
/// The serve window it carries is the reader's exemption (ruling (7)(b)(ii)
/// rule (2)); the row's `webdav_enabled` is never read.
fn summary_lineage(
    summary: &FolderSummary,
    cfg: &FoldersConfig,
) -> fauna_core::folder_keys::SetNonceLineage {
    let owned = summary.role.as_deref() != Some("member");
    let owner_name = owned.then_some(summary.name.as_str());
    let channel = fauna_client_folders::custody_channel_for(summary, cfg)
        .ok()
        .flatten();
    fauna_client_folders::custody::set_lineage(cfg, owner_name, channel.as_ref())
}

/// Resolve a **cross-nest** set's binding from its holder's custody record —
/// `None` = refused. The set has no row on this nest; every input its
/// transport needs is the `ForeignFolder` record the member's app wrote at
/// share-accept and refreshes on every federated read reply
/// (`on-demand-files.md` § Shared sets on a capability host → *One mechanism*,
/// question 2).
async fn resolve_foreign_binding(
    params: &EngineParams,
    channel: [u8; 32],
) -> Option<ResolvedBinding> {
    let folder_ref = params.folder_ref;
    let folder_id = folder_ref.to_wire();

    // Custody load is unconditional: a foreign set is always bound, and its
    // record is its row. Unreadable custody refuses rather than building
    // keyless — without the record there is no home nest to dial, so there is
    // nothing to build; the host's next edge re-reads it.
    let Some(cfg) = load_custody_retry(&*params.folder_keys).await else {
        tracing::error!(
            "folder {folder_id}: custody unreadable — a cross-nest set's transport is its \
             custody record; refusing to run engine (fail closed). The host rebuilds it at its \
             next edge."
        );
        return None;
    };
    let Some(record) = fauna_client_folders::custody::find_foreign_set(&cfg, &channel) else {
        tracing::error!(
            "folder {folder_id}: no custody record for this cross-nest set (left, or this \
             account was removed from it); refusing to run engine"
        );
        return None;
    };
    // The one resolver the agent's own custody read runs, so the two can never
    // key a foreign set differently. It skips a name-less record (a peer-supplied
    // record that omits the set name) — not bindable, the fail-safe direction.
    let Some(keys) = resolve_foreign_engine_key_bindings(&cfg)
        .into_iter()
        .find(|k| FolderRef::parse(&k.folder_id) == Some(folder_ref))
    else {
        tracing::error!(
            "folder {folder_id}: the custody record carries no set name (a peer-supplied record \
             that omits the set name), so the set is not bindable; refusing to run engine"
        );
        return None;
    };
    let (Some(home_nest_url), Some(channel_id_hex)) =
        (keys.home_nest_url.clone(), keys.channel_id_hex.clone())
    else {
        tracing::error!(
            "folder {folder_id}: a cross-nest resolution without its routing pair; refusing"
        );
        return None;
    };

    // Cross-nest byte-plane trust (`security.md` § Transport trust): the byte
    // plane dials the HOME nest directly — a nest this member holds no account
    // on — so before the client below is built, graduate an SPKI pin from the
    // record's `home_nest_actor_id` (the pre-identity `fauna.auth.nest_handshake`).
    // `https` only (plain-http loopback has no TLS); an absent actor id keeps the
    // `RequireWebPki` floor (never weaker). A refusal is fail-closed and loud:
    // no engine, rebuilt at the next edge.
    if home_nest_url.starts_with("https://")
        && let Some(home_nest_actor_id) = keys.home_nest_actor_id.as_deref()
        && let Err(e) = graduate_home_nest_pin(&home_nest_url, home_nest_actor_id).await
    {
        tracing::error!(
            "folder {folder_id}: cross-nest byte-plane pin graduation against {home_nest_url} \
             failed; refusing to run engine: {e:#}"
        );
        return None;
    }

    let held = keys
        .content_keys
        .as_ref()
        .map(FolderContentKeys::current_version);
    // The set's owner for the reader's writer check: the channel's
    // MLS-recorded owner, on a host that carries an MLS engine.
    let owner_actor_id = params.mls.as_deref().and_then(|mls| {
        use fauna_client_folders::FolderChannelOwners as _;
        crate::config::MlsChannelOwners(mls)
            .folder_channel_owner(&channel)
            .map(|a| a.0)
    });
    Some(ResolvedBinding {
        owner_actor_id,
        // A cross-nest set is never this account's own.
        member: true,
        // The member's received copy of the owner's serve stamps (the owner's
        // envelope, ruling (7)(b)(ii) rule (4)) — a cross-nest member admits
        // the served era exactly as a same-nest one does.
        webdav_served: keys.webdav_served(),
        // The member's received copy of the owner's nonce — what its records
        // are signed under, as for a same-nest shared set.
        set_nonce: keys.set_nonce,
        retired_set_nonces: keys.retired_set_nonces.clone(),
        set_nonce_minted_by: keys.set_nonce_minted_by.map(|a| a.0),
        retired_lineage: keys
            .retired_lineage
            .iter()
            .map(|r| (r.nonce, r.minted_by.map(|a| a.0)))
            .collect(),
        // A member's set: the take-over is the owner's alone.
        adoption_marker: None,
        folder: keys.folder,
        mls_group_id: keys.mls_group_id,
        content_keys: keys.content_keys,
        // A foreign set's audience is the home nest's fact, and the record does
        // not carry it — so a foreign engine always seals (the fail-safe
        // direction).
        public_audience: false,
        retired_content_keys: None,
        // A member never re-seals onto its own root: nothing to hold.
        served_era_hold: false,
        // The set's policy lives on the home nest's row; the record does not
        // carry it, so the default applies.
        conflict_policy: fauna_core::format::ConflictPolicy::default(),
        // The residency the home nest stamped on the custody record (the
        // Welcome seeds it, every federated read reply refreshes it): the
        // seat arms the upload skip and the holder-keeps gate from it as a
        // same-nest seat does from its row. A record no home nest has stamped
        // is unknown — never *full* — and a flip reaches a resident engine as
        // a basis change at the custody edge (`BindingBasis::of_foreign`).
        metadata_only_residency: record.metadata_only_residency(),
        // Decision 2's pre-seal hold, armed from the floor the record carries
        // (the home nest's stamp off the federated content-key read, refreshed
        // on every commit poll): a foreign engine behind the floor holds its
        // write locally, ahead of the home nest's `stale_content_key` refusal,
        // and a floor move reaches it as a basis change at the custody edge. A
        // record without a stamp arms nothing — the refusal alone, as before.
        seal_floor: SealFloor::Floor(record.content_key_floor),
        basis: BindingBasis::of_foreign(record, held),
        foreign: Some(ForeignTransport {
            home_nest_url,
            channel_id_hex,
        }),
        // The record's access is the home nest's advisory stamp, and reading
        // it as a reader costs at most a refused write: absent or unknown is a
        // reader. A host over a user-writable directory keeps today's shape
        // (its bind gesture verified the grant at the home nest).
        read_only: params.reader_hosting == ReaderHosting::ReadOnly
            && record.access.as_deref() != Some("writer"),
    })
}

/// The reader binding's `account_predecessors`: the attested ids, then any id
/// a paired key names that they lack — one list, in the order given, without
/// repeats. Both sources are the identity-holding app's own word over the
/// same pipe; neither is nest-asserted.
fn binding_predecessors(
    attested: &[[u8; 32]],
    keys: &[fauna_core::file_download::PredecessorSealKey],
) -> Vec<[u8; 32]> {
    let mut ids: Vec<[u8; 32]> = Vec::with_capacity(attested.len());
    for id in attested
        .iter()
        .copied()
        .chain(keys.iter().filter_map(|k| k.actor_id.map(|a| a.0)))
    {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// Assemble the [`SyncEngine`] for a resolved `binding` — the construction half
/// of [`build_engine`], and the ONE construction path: no network, no
/// decision, only wiring. Production reaches it only through [`build_engine`];
/// a test that needs an engine without a control plane assembles one from a
/// hand-made binding.
///
/// Chunk crypto follows the binding: an **owner-only** set seals under the
/// owner's `BackupKey` (the engine refuses a keyless owner-only upload); a
/// **bound** set seals every chunk under its current content key, or fails
/// closed when it holds none — never plaintext. A **cross-nest** set's byte
/// plane dials its home nest under a write-token bearer — a read-token bearer
/// when the binding is `read_only` — and both control-plane kinds relay
/// through the own nest.
pub fn assemble_engine(params: EngineParams, binding: ResolvedBinding) -> Result<BuiltEngine> {
    let EngineParams {
        state_dir,
        watch_dir,
        folder_ref,
        device_id,
        device_label: _,
        auth,
        nest_rpc,
        mls,
        credential,
        progress_tx,
        predecessor_backup_keys,
        predecessor_actor_ids,
        learned_predecessors,
        access_gate,
        change_signer,
        folder_keys,
        reader_hosting: _,
    } = params;
    let ResolvedBinding {
        folder,
        mls_group_id,
        content_keys,
        public_audience,
        retired_content_keys,
        served_era_hold,
        conflict_policy,
        metadata_only_residency,
        seal_floor,
        basis,
        foreign,
        set_nonce,
        retired_set_nonces,
        set_nonce_minted_by,
        retired_lineage,
        adoption_marker,
        owner_actor_id,
        member,
        webdav_served,
        read_only,
    } = binding;
    // This account — the owner of a same-nest set whose binding names none.
    let auth_actor_id = auth.actor_id();
    // The bound set's custody channel — what a re-fetch request names.
    let custody_channel = mls_group_id
        .as_deref()
        .map(|group| fauna_mls::types::ChannelId::from_group_id(group).0);

    let db =
        SyncDb::open(folder_ref.state_db_path(&state_dir)).context("open the set's state db")?;
    // Shared with the cross-nest byte-plane bearer below, which is built before
    // the engine: a demoted writer meets the refusal at the mint, on its first
    // upload byte, and the engine parks on the same flag.
    let access_gate = access_gate.unwrap_or_else(AccessGate::new);

    // A cross-nest set's chunks and manifests live on its HOME nest, where this
    // actor has no session — so its byte plane is re-pointed there under a
    // `WriteTokenBearer`, which mints short-lived write tokens via
    // `fauna.folders.write_token.get` on the OWN nest (relayed to the home nest
    // behind its foreign-member + `access == 'writer'` gate). A demotion
    // therefore surfaces as a typed mint refusal — the enforcement point. Its
    // SPKI pin is keyed per host, so the home nest gets its own http client
    // (graduated by the resolution before this runs).
    //
    // A READER's engine (the binding's `read_only`) takes the read twin
    // instead — `fauna.folders.read_token.get`, behind the member gate alone —
    // because its chunk and manifest GETs take a bearer too, and the write mint
    // refuses a reader: it would park as a demoted writer before its first
    // read. Its engine refuses every write before the byte plane, so the read
    // token's refusal at every bulk write route is never met.
    let client = match &foreign {
        Some(transport) => {
            let mint = if read_only {
                crate::write_token_bearer::folder_read_token_bearer
            } else {
                crate::write_token_bearer::folder_write_token_bearer
            };
            let byte_bearer: Arc<dyn BearerSource> = Arc::new(mint(
                Arc::clone(&nest_rpc),
                transport.home_nest_url.clone(),
                transport.channel_id_hex.clone(),
                Arc::clone(&access_gate),
            ));
            let byte_auth = Arc::new(AuthClient::bearer_only(
                transport.home_nest_url.clone(),
                auth.actor_id(),
                byte_bearer,
                pinned_http_client(&transport.home_nest_url),
            ));
            SyncClient::new(byte_auth, &device_id)
        }
        None => SyncClient::new(auth, &device_id),
    };

    // The seat's mode is NOT resolved here. Resolution happens exactly once, in
    // `SyncEngine::refresh_sync_mode` (role-first via the shared
    // `config::resolve_device_mode`), which every drive path runs before its
    // first pull. The constructor value below is only the pre-resolution
    // starting position: the persisted last authoritative answer when one
    // exists, else the bidirectional default — and no pull can happen before
    // the refresh corrects it.
    let mode = db
        .get_cached_sync_mode()
        .ok()
        .flatten()
        .and_then(|s| crate::config::SyncMode::from_cache_str(&s))
        .unwrap_or_default();

    // Every engine uploads, so every engine MUST honor `.faunaignore` — an
    // ignore list that silently does not apply is worse than none, so an
    // unreadable one fails the build. (`IgnoreMatcher::load` reads a cloud-files
    // placeholder root's "not there yet" error as "no ignore file"; the scan
    // skips every dotfile, so `.faunaignore` itself is never a placeholder.)
    let ignore = IgnoreMatcher::load(&watch_dir).context("load .faunaignore for the folder")?;
    let concurrency = Arc::new(AdaptiveConcurrency::fixed(MAX_CONCURRENT_CHUNKS));
    let transfer_pool = TransferPool::new(concurrency, progress_tx);

    let held_version = content_keys
        .as_ref()
        .map(FolderContentKeys::current_version);
    let change_signer = effective_change_signer(change_signer, &credential);
    let backup_key = owner_backup_key(&credential);
    if backup_key.is_none() {
        tracing::error!(
            "folder {}: owner content-seal root unavailable (identity seed failed to decode) — \
             owner-only uploads will fail closed rather than upload plaintext",
            fauna_core::log_redact::log_folder_name(&folder)
        );
    }

    // Owned by value (not `Arc`): `SyncEngine` is `Send` but not `Sync` (rusqlite
    // `Connection`), and it is driven by exactly one loop.
    let mut engine = SyncEngine::new(
        watch_dir,
        db,
        client,
        Some(folder),
        device_id,
        mls,
        None,
        backup_key.map(Into::into),
        mls_group_id,
        content_keys,
        conflict_policy,
        default_format_registry(),
        ignore,
        PARALLEL_DOWNLOADS,
        transfer_pool,
        // The control plane ALWAYS rides the caller's OWN nest, foreign or not —
        // that is what a cross-nest relay is: this actor has a session only here.
        nest_rpc,
        mode,
    )
    .with_public_audience(public_audience)
    .with_residency_reading(metadata_only_residency)
    .with_seal_floor(seal_floor)
    .with_read_only(read_only);
    engine.set_retired_content_keys(retired_content_keys);
    engine.set_served_era_hold(served_era_hold);
    engine.set_change_signer(change_signer, set_nonce);
    engine.set_retired_set_nonces(retired_set_nonces);
    engine.set_adoption_marker(adoption_marker);
    // Third-party deposits park sealed to the OWNER's recipient key, so only a
    // set this account owns on its own nest adopts them (`file-sync.md`
    // § Third-party deposit ingress); a member sees the adopted file as any
    // other. The same custody source answers the MSEK the key derives from.
    if let FolderRef::Local(folder_id) = folder_ref
        && foreign.is_none()
        && !member
        && !read_only
        && owner_actor_id.is_none_or(|o| o == auth_actor_id)
    {
        engine.set_deposit_inbox(folder_id, Arc::clone(&folder_keys));
    }
    // A member's engine that refuses a row `signature_invalid` asks the
    // process holding the MLS group to re-fetch the envelope (ruling (11)(b)):
    // a stamp in the store's device-local meta, which that process's
    // custody-ingest sink reads. The engine asks only on a set this account
    // does not own; a host whose custody source keeps no device-local state
    // drops the ask.
    if let Some(channel) = custody_channel {
        engine.set_custody_refetch_request(Arc::new(move || {
            let folder_keys = Arc::clone(&folder_keys);
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return;
            };
            runtime.spawn(async move {
                let now = fauna_core::data::Timestamp::now().0;
                if let Err(e) = folder_keys.request_refetch(&channel, now).await {
                    tracing::debug!("custody re-fetch request not recorded: {e:#}");
                }
            });
        }));
    }
    // The reader half (ruling (3)): every row this engine consumes is judged
    // against the same nonce, the set's owner and its serve flag. A member's
    // owner — same-nest or cross-nest — is the MLS-recorded one where the
    // host holds it; where it holds none the binding names no owner and the
    // reader takes it off the roster's owner row (ruling (11)(c)), its roster
    // leg relaying to the home nest for a cross-nest set.
    let owner = owner_actor_id.or_else(|| (foreign.is_none() && !member).then_some(auth_actor_id));
    // What the host was handed, led by whatever an earlier build's walk
    // already proved (in order: a walk is the chain's nearest run).
    let account_predecessors = learned_predecessors.chain_with(&binding_predecessors(
        &predecessor_actor_ids,
        &predecessor_backup_keys,
    ));
    engine.set_learned_predecessors(learned_predecessors);
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce,
        owner,
        webdav_served,
        // The own-account source (ruling (8)(b), source (ii)): a row signed
        // as one of this account's attested predecessors verifies as this
        // account's, and every open of it is bounded to that identity's root
        // and earlier ones (ruling (8)(c), the engine's `record_signer_of`).
        account: Some(auth_actor_id),
        // The cut (`writer-signed-change-records.md` ruling (11)(c)): the live
        // nonce's minter, the lineage with each nonce's, and — on a set this
        // account owns — the owner's chain in order, nearest first (the
        // attested walk is; a member's reader takes its chain off the
        // roster's owner row instead).
        live_minted_by: set_nonce_minted_by,
        retired_set_nonces: retired_lineage,
        owner_chain: if owner == Some(auth_actor_id) {
            account_predecessors.clone()
        } else {
            Vec::new()
        },
        account_predecessors,
    });
    engine.set_access_gate(access_gate);
    // A successor's corpus is still sealed under the identities it succeeded
    // from; these open it. Read-only — `content_seal_root` reads `backup_key`
    // alone, so nothing this engine uploads can land back under a retired root.
    if !predecessor_backup_keys.is_empty() {
        engine.set_predecessor_backup_keys(predecessor_backup_keys);
    }
    // Cross-nest: relay BOTH control-plane kinds through the own nest to the
    // home nest. Without the read half (`changes.list`) a bound foreign set polls
    // its own nest's log — which holds no rows for a set it does not claim — and
    // so silently never pulls the owner's edits.
    if let Some(transport) = foreign {
        engine.set_foreign_routing(transport.home_nest_url, transport.channel_id_hex);
    }

    Ok(BuiltEngine {
        engine,
        basis,
        held_version,
        read_only,
    })
}

/// Build a **minimal, one-shot restore engine** — for the client-side full-restore
/// walk ([`SyncEngine::restore_snapshot_files_to_dir`]) driven by the macOS
/// `MacRestoreView` (`docs/goal/behavior/backup-restore.md` § 4).
///
/// A restore only reads: it fetches manifests + chunks and decrypts them under the
/// owner `BackupKey`, and never touches the state DB. So — unlike [`build_engine`]
/// — it deliberately does **not** open the real `fsid-<ref>.db` (an in-memory throwaway
/// suffices, so a live-syncing set's SyncDb is never disturbed) and does **not** run
/// the `fauna.folders.list` binding resolution (owner-only restore seals under the
/// identity-derived `BackupKey`; a plaintext manifest passes through, a sealed manifest with no key fails
/// closed — [`SyncEngine::download_file_bytes_by_manifest`]). Bound (cross-user
/// shared) content-key restore is out of scope for this owner-restore surface.
///
/// `secret_hex` is the owner's identity seed (the sole `BackupKey` holder); an
/// undecodable seed yields an engine whose `effective_backup_key()` is `None`, so a
/// sealed snapshot then fails closed rather than emitting ciphertext.
pub fn build_restore_engine(
    auth: Arc<AuthClient>,
    nest_rpc: Arc<NestClient>,
    device_id: [u8; 32],
    secret_hex: &str,
) -> SyncEngine {
    let db = SyncDb::open_in_memory().expect("in-memory SyncDb");
    let client = SyncClient::new(auth, &device_id);
    let concurrency = Arc::new(AdaptiveConcurrency::fixed(MAX_CONCURRENT_CHUNKS));
    let transfer_pool = TransferPool::new(concurrency, None);
    let backup_key = fauna_core::hex32::decode(secret_hex)
        .ok()
        .map(|secret| fauna_core::crypto::BackupKey::derive(&secret));

    SyncEngine::new(
        // No watch dir — the restore method writes to the caller's output dir
        // directly; the engine's `watch_dir` is never read by the download walk.
        PathBuf::new(),
        db,
        client,
        None, // folder: the walk is manifest-addressed, not set-scoped
        device_id,
        None, // mls: the download walk never touches MLS
        None, // epoch_secret
        backup_key.map(Into::into),
        None, // mls_group_id (owner-only restore, not a bound set)
        None, // content_keys (owner-only restore)
        fauna_core::format::ConflictPolicy::default(),
        default_format_registry(),
        IgnoreMatcher::default(),
        PARALLEL_DOWNLOADS,
        transfer_pool,
        nest_rpc,
        // Sync: this one-shot restore engine only ever downloads into the
        // caller's output dir — it runs no `apply_remote_changes` and so reaches
        // no delete arm at all. The delete-applying default is the honest value
        // for an engine that applies nothing.
        crate::config::SyncMode::Sync,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sign-out sweep takes the actor's scope, `device.db` included; the
    /// install dir survives it. The next sign-in must come back to the same id.
    #[test]
    fn a_sign_out_sweep_then_sign_in_comes_back_to_the_same_device_id() {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("sync");
        let scope = install.join("aa".repeat(32));
        let actor = [0xaa; 32];

        let before = load_device_id_for_actor(&install, &scope, &actor).unwrap();
        std::fs::remove_dir_all(&scope).unwrap();
        let after = load_device_id_for_actor(&install, &scope, &actor).unwrap();

        assert_eq!(
            before, after,
            "the returning sign-in registered a new device"
        );
    }

    #[test]
    fn two_accounts_on_one_install_never_share_a_device_id() {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("sync");
        let a = load_device_id_for_actor(&install, &install.join("a"), &[0xaa; 32]).unwrap();
        let b = load_device_id_for_actor(&install, &install.join("b"), &[0xbb; 32]).unwrap();
        assert_ne!(a, b);
    }

    /// A persisted id — and the e2e session door's forced id — must keep the
    /// row it already registered.
    #[test]
    fn a_persisted_device_id_outranks_the_derivation() {
        let tmp = tempfile::tempdir().unwrap();
        let install = tmp.path().join("sync");
        let scope = install.join("a");
        std::fs::create_dir_all(&scope).unwrap();
        let forced = "0123456789abcdef".repeat(4);
        store_device_id_hex(&scope, &forced).unwrap();

        let id = load_device_id_for_actor(&install, &scope, &[0xaa; 32]).unwrap();

        assert_eq!(fauna_core::hex32::encode(&id), forced);
        assert!(
            !install.join(INSTALL_DEVICE_SECRET_FILE).exists(),
            "a persisted id needs no secret; minting one is a write the read path does not owe"
        );
    }

    #[test]
    fn the_install_device_secret_is_minted_once_and_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let first = load_or_create_install_device_secret(tmp.path()).unwrap();
        let second = load_or_create_install_device_secret(tmp.path()).unwrap();
        assert_eq!(first, second);
        assert_ne!(first, [0u8; 32]);
    }

    #[test]
    fn seed_and_pre_derived_backup_key_credentials_yield_the_same_owner_seal_root() {
        // The security equivalence that makes the seed-less File Provider path
        // safe: a host handed `BackupKey::derive(&seed)` seals owner-only content
        // under the *exact same* root the identity-holding app derives from the
        // seed itself — so a seed-less extension is byte-for-byte interchangeable
        // with the seed-holding app on the owner-only seal path.
        let seed = [0x37u8; 32];
        let via_seed = owner_backup_key(&EngineCredential::Seed(hex::encode(seed)))
            .expect("valid seed hex derives a key");
        let via_key = owner_backup_key(&EngineCredential::BackupKey(BackupKey::derive(&seed)))
            .expect("a pre-derived key is always present");
        assert_eq!(
            via_seed.to_bytes(),
            via_key.to_bytes(),
            "the seed-less path must produce the identical owner content-seal root"
        );
        // A malformed seed hex fails closed (None) — never a silent wrong key.
        assert!(owner_backup_key(&EngineCredential::Seed("not-hex".into())).is_none());
    }

    /// The decision as a host over a user-writable directory asks it — every
    /// host but a control-inverted one.
    fn decide(
        list_ok: bool,
        summary: Option<&FolderSummary>,
        config: Option<&FoldersConfig>,
        trusted_owner: Option<&fauna_core::identity::ActorId>,
        memory: AttestationMemory,
    ) -> EngineContentDecision {
        decide_engine_content_binding(
            list_ok,
            summary,
            config,
            trusted_owner,
            memory,
            ReaderHosting::Refuse,
        )
    }

    fn summary(name: &str) -> FolderSummary {
        FolderSummary {
            id: 0,
            name: name.to_string(),
            retention_policy: None,
            cached_snapshot_count: 0,
            cached_total_bytes: 0,
            cached_last_snapshot_at: None,
            include_paths: None,
            exclude_paths: None,
            mls_group_id: None,
            role: None,
            owner_handle: None,
            webdav_enabled: false,
            ..Default::default()
        }
    }

    /// A summary bound to `raw_gid` (stored as hex), atop the owner-only builder.
    fn bound_summary(name: &str, raw_gid: &[u8]) -> FolderSummary {
        let mut s = summary(name);
        s.mls_group_id = Some(hex::encode(raw_gid));
        s
    }

    /// A shared-*with*-me member summary (`role == "member"`) bound to `raw_gid`,
    /// carrying the caller's `access` grant. Multi-writer Phase 1: a `"writer"`
    /// binds like the owner, a reader (or absent access) is refused by the guard.
    fn member_bound_summary(name: &str, raw_gid: &[u8], access: Option<&str>) -> FolderSummary {
        let mut s = bound_summary(name, raw_gid);
        s.role = Some("member".to_string());
        s.access = access.map(str::to_string);
        s
    }

    /// The build resolves a set's row by IDENTITY: two rows wearing one name
    /// resolve to their own ids, a `Foreign` ref (no row on this nest by
    /// construction) matches nothing, and an id the list does not carry is
    /// absent — never a same-named neighbour.
    #[test]
    fn summary_for_ref_matches_by_identity_never_by_name() {
        let mut first = summary("docs");
        first.id = 1;
        let mut second = bound_summary("docs", b"raw-openmls-group-id");
        second.id = 2;
        let list = vec![first, second];

        assert_eq!(
            summary_for_ref(&list, FolderRef::Local(1)).map(|s| s.id),
            Some(1)
        );
        assert_eq!(
            summary_for_ref(&list, FolderRef::Local(2)).map(|s| s.id),
            Some(2)
        );
        assert!(summary_for_ref(&list, FolderRef::Local(3)).is_none());
        assert!(
            summary_for_ref(&list, FolderRef::Foreign([0u8; 32])).is_none(),
            "a cross-nest set has no row in this nest's own list"
        );
    }

    #[test]
    fn binding_refuses_when_list_unavailable() {
        // List fetch failed → binding indeterminate → fail closed (a set we can't
        // classify might be bound; running it unbound would risk plaintext-sealing
        // a shared set).
        assert_eq!(
            decide(false, None, None, None, AttestationMemory::default()),
            EngineContentDecision::Refuse
        );
    }

    #[test]
    fn binding_unbound_for_absent_or_owner_only_set() {
        // Absent from a *successful* list (deleted / unshared) → owner-only, run
        // unbound.
        assert_eq!(
            decide(true, None, None, None, AttestationMemory::default()),
            EngineContentDecision::Run(None, None, sealed())
        );
        // Present but no group id (owner-only) → run unbound (no config load).
        let s = summary("owner-only");
        assert_eq!(
            decide(true, Some(&s), None, None, AttestationMemory::default()),
            EngineContentDecision::Run(None, None, sealed())
        );
    }

    #[test]
    fn binding_bound_without_config_is_bound_keyless_fail_closed() {
        // Bound set, custody unavailable (config load failed) → bound-keyless
        // (Some(gid), None): the engine fails closed, NEVER (None, None) (which
        // would seal shared content in plaintext).
        let raw = b"raw-openmls-group-id".to_vec();
        let s = bound_summary("shared-docs", &raw);
        assert_eq!(
            decide(true, Some(&s), None, None, AttestationMemory::default()),
            EngineContentDecision::Run(Some(raw), None, sealed())
        );
    }

    #[test]
    fn binding_refuses_bound_set_with_corrupt_group_id() {
        // Bound but the projection is not valid hex → cannot build the bound
        // marker → refuse (never downgrade a bound set to unbound).
        let mut s = summary("corrupt");
        s.mls_group_id = Some("not-valid-hex-zz".to_string());
        assert_eq!(
            decide(true, Some(&s), None, None, AttestationMemory::default()),
            EngineContentDecision::Refuse
        );
    }

    #[test]
    fn binding_refuses_reader_member_set() {
        // A set shared *with* the caller as a READER is unbindable (multi-writer
        // Phase 1): a bound folder whose edits could not upload breaches
        // file-sync.md's iron rule, so the engine refuses rather than half-sync —
        // even though a reader holds the keys (they read via Media, not a sync
        // engine). Fail-closed: absent access ⇒ reader ⇒ refuse. Without the guard
        // this same bound member summary would resolve to bound-keyless Run.
        let raw = b"raw-group-id-reader".to_vec();
        for access in [None, Some("reader")] {
            let s = member_bound_summary("shared-reader", &raw, access);
            assert_eq!(
                decide(true, Some(&s), None, None, AttestationMemory::default()),
                EngineContentDecision::Refuse,
                "a {access:?}-access member set must not run a sync engine"
            );
        }
    }

    /// A control-inverted host builds a reader's set instead of refusing it:
    /// the reader guard exists for hosts over a user-writable directory, and
    /// such a host has none. The set resolves exactly as a writer's does —
    /// bound, keyed from the member's own custody — and the caller marks the
    /// engine read-only. Every other refusal still stands.
    #[test]
    fn binding_reader_member_set_runs_on_a_control_inverted_host() {
        let raw = vec![7u8; 16];
        let read_only = |summary: &FolderSummary, config: Option<&FoldersConfig>| {
            decide_engine_content_binding(
                true,
                Some(summary),
                config,
                None,
                AttestationMemory::default(),
                ReaderHosting::ReadOnly,
            )
        };
        for access in [None, Some("reader"), Some("bogus")] {
            let s = member_bound_summary("shared", &raw, access);
            assert!(s.is_reader_member());
            assert_eq!(
                read_only(&s, None),
                EngineContentDecision::Run(Some(raw.clone()), None, sealed()),
                "access {access:?}: bound-keyless without custody, never refused as a reader"
            );
            assert_eq!(
                decide(true, Some(&s), None, None, AttestationMemory::default()),
                EngineContentDecision::Refuse,
                "access {access:?}: a host over a user-writable directory still refuses"
            );
        }
        // A writer is not a reader on either host.
        let writer = member_bound_summary("shared", &raw, Some("writer"));
        assert!(!writer.is_reader_member());
        assert_eq!(
            read_only(&writer, None),
            decide(
                true,
                Some(&writer),
                None,
                None,
                AttestationMemory::default()
            ),
        );
        // The policy lifts the reader guard and nothing else: a corrupt group
        // id still refuses.
        let mut corrupt = member_bound_summary("shared", &raw, None);
        corrupt.mls_group_id = Some("zz".into());
        assert_eq!(read_only(&corrupt, None), EngineContentDecision::Refuse);
    }

    #[test]
    fn binding_writer_member_set_is_bound_like_the_owner() {
        // A WRITER member IS bindable: the guard passes and the set resolves
        // through the ordinary bound path (bound-keyless here, no custody config),
        // so a writer seals under the set's content key exactly as the owner does.
        let raw = b"raw-group-id-writer".to_vec();
        let s = member_bound_summary("shared-writer", &raw, Some("writer"));
        assert_eq!(
            decide(true, Some(&s), None, None, AttestationMemory::default()),
            EngineContentDecision::Run(Some(raw), None, sealed()),
            "a writer member set must bind like the owner (bound-keyless without custody)"
        );
    }

    /// A WebDAV-served, group-less summary atop the owner-only builder.
    fn served_summary(name: &str) -> FolderSummary {
        let mut s = summary(name);
        s.webdav_enabled = true;
        s
    }

    /// An empty custody for the custody-resolution tests.
    fn empty_cfg() -> FoldersConfig {
        FoldersConfig::default()
    }

    /// Rule (5)'s re-seal hold: a keyed, stamp-less entry the roster flags
    /// served holds; the same entry unflagged, or once the owner's serve-on
    /// stamps it (and after a stamped serve-off), does not.
    #[test]
    fn the_reseal_holds_only_for_a_keyed_stamp_less_flagged_set() {
        use fauna_client_folders::custody::{record_new_set, serve_off, serve_on};
        let channel = fauna_core::folder_keys::serve_custody_channel_id("pre-build");
        let mut cfg = empty_cfg();
        record_new_set(&mut cfg, channel, [0x42; 32], 1_000);
        let flagged = served_summary("pre-build");
        assert!(served_era_hold(&flagged, &cfg, true));
        assert!(
            !served_era_hold(&flagged, &cfg, false),
            "no retired candidate, nothing to walk"
        );
        assert!(
            !served_era_hold(&summary("pre-build"), &cfg, true),
            "the roster says unserved"
        );
        let mut member = flagged.clone();
        member.role = Some("member".into());
        assert!(!served_era_hold(&member, &cfg, true));
        assert!(serve_on(&mut cfg, &channel, 2_000));
        assert!(serve_off(&mut cfg, &channel, 3_000));
        assert!(
            !served_era_hold(&flagged, &cfg, true),
            "a stamped serve-off is the owner's word: the walk runs"
        );
    }

    /// Ruling (7)(b)(ii) rule (2), at the engine's build: the reader's served
    /// exemption is custody's serve window. An owned unshared set the roster
    /// flags served, whose custody is keyed and stamp-less, exempts nothing
    /// (a planted pseudo-device row is judged `Unsigned`); a same-nest member
    /// of a served shared set admits the owner's pseudo-device rows on its
    /// received copy's window, and not before the owner served it.
    #[test]
    fn the_readers_served_window_is_custodys_never_the_roster_flag() {
        use fauna_client_folders::custody::{record_new_set, serve_on};
        let mut cfg = empty_cfg();
        let flagged = served_summary("flagged");
        record_new_set(
            &mut cfg,
            fauna_core::folder_keys::serve_custody_channel_id("flagged"),
            [0x42; 32],
            1_000,
        );
        assert!(!summary_lineage(&flagged, &cfg).webdav_served());

        let raw = b"raw-group-served".to_vec();
        let mut member = member_bound_summary("shared-served", &raw, Some("writer"));
        member.webdav_enabled = true;
        let channel = fauna_core::folder_keys::channel_id_for_group(&raw);
        record_new_set(&mut cfg, channel, [0x43; 32], 1_000);
        assert!(
            !summary_lineage(&member, &cfg).webdav_served(),
            "the roster's flag alone admits nothing"
        );
        assert!(serve_on(&mut cfg, &channel, 2_000));
        member.webdav_enabled = false;
        assert!(
            summary_lineage(&member, &cfg).webdav_served(),
            "the owner's stamp in the received copy admits the served era"
        );
    }

    #[test]
    fn binding_served_unshared_with_custody_runs_group_less_content_keyed() {
        // Served set, custody resolves at the serve pseudo-channel → the engine
        // runs (None, Some(keys)): seals under `current`, no bound-marker.
        let s = served_summary("served-docs");
        let mut cfg = empty_cfg();
        let pseudo = fauna_core::folder_keys::serve_custody_channel_id("served-docs");
        fauna_client_folders::custody::record_new_set(&mut cfg, pseudo, [0x42; 32], 1_000);
        // Keyed and stamp-less: the roster's flag alone binds nothing — the
        // set runs owner-only (ruling (7)(b)(ii) rule (2)).
        assert_eq!(
            decide(
                true,
                Some(&s),
                Some(&cfg),
                None,
                AttestationMemory::default(),
            ),
            EngineContentDecision::Run(None, None, sealed()),
            "a nest flagging an unserved set served content-keys nothing"
        );
        fauna_client_folders::custody::serve_on(&mut cfg, &pseudo, 2_000);
        match decide(
            true,
            Some(&s),
            Some(&cfg),
            None,
            AttestationMemory::default(),
        ) {
            EngineContentDecision::Run(
                None,
                Some(keys),
                DeclassificationVerdict {
                    unsealed: false, ..
                },
            ) => {
                assert_eq!(keys.current_key(), &[0x42; 32]);
            }
            other => panic!("expected group-less content-keyed Run, got {other:?}"),
        }
    }

    #[test]
    fn binding_bound_with_custody_runs_content_keyed_under_the_current_generation() {
        // The arm a capability host now reaches (on-demand-files.md § Shared sets
        // on a capability host, decision 1): custody read under the host's own
        // wraps holds the set's generations → the engine runs bound AND keyed,
        // sealing under the current generation. Custody is credential-blind —
        // the same fold a seed-holding app reads — so the pin is the same for
        // both credentials.
        let raw = b"raw-openmls-group-id".to_vec();
        let channel = fauna_mls::types::ChannelId::from_group_id(&raw).0;
        let mut cfg = empty_cfg();
        fauna_client_folders::custody::record_new_set(&mut cfg, channel, [0x42; 32], 1_000);
        fauna_client_folders::custody::rotate_set(&mut cfg, &channel, [0x43; 32], 2_000);
        for s in [
            bound_summary("shared", &raw),
            member_bound_summary("shared", &raw, Some("writer")),
        ] {
            match decide(
                true,
                Some(&s),
                Some(&cfg),
                None,
                AttestationMemory::default(),
            ) {
                EngineContentDecision::Run(
                    Some(gid),
                    Some(keys),
                    DeclassificationVerdict {
                        unsealed: false, ..
                    },
                ) => {
                    assert_eq!(gid, raw);
                    assert_eq!(keys.current_version(), 2);
                    assert_eq!(keys.current_key(), &[0x43; 32]);
                    assert_eq!(keys.key_for(1), Some(&[0x42; 32]), "history retained");
                }
                other => panic!("expected a keyed bound Run, got {other:?}"),
            }
        }
    }

    // ── Re-resolve edges (on-demand-files.md § Shared sets on a capability
    //    host, decision 2) ──

    #[test]
    fn floor_ahead_only_when_a_floor_exceeds_the_held_generation() {
        assert!(!floor_ahead(Some(1), None), "no floor established");
        assert!(
            !floor_ahead(None, None),
            "owner-only: no floor, no generation"
        );
        assert!(!floor_ahead(Some(2), Some(2)));
        assert!(!floor_ahead(Some(3), Some(2)));
        assert!(floor_ahead(Some(1), Some(2)), "rotated past what is held");
        assert!(
            floor_ahead(None, Some(1)),
            "a keyless bound engine is behind any floor"
        );
    }

    /// [`edge_verdict`] over the row a same-nest edge reads.
    fn verdict(
        basis: &BindingBasis,
        held: Option<u64>,
        now: Option<&FolderSummary>,
    ) -> EdgeVerdict {
        edge_verdict(basis, held, now.map(BindingBasis::of).as_ref())
    }

    #[test]
    fn edge_keeps_an_engine_built_on_the_row_it_reads_now() {
        let raw = b"raw-group".to_vec();
        let mut row = bound_summary("shared", &raw);
        row.content_key_floor = Some(2);
        let basis = BindingBasis::of(&row);
        assert_eq!(verdict(&basis, Some(2), Some(&row)), EdgeVerdict::Keep);
        // Stats are not binding facts.
        let mut busier = row.clone();
        busier.cached_total_bytes = 1 << 20;
        assert_eq!(verdict(&basis, Some(2), Some(&busier)), EdgeVerdict::Keep);
        // An owner-only set with no floor keeps its keyless engine.
        let own = summary("own");
        assert_eq!(
            verdict(&BindingBasis::of(&own), None, Some(&own)),
            EdgeVerdict::Keep
        );
    }

    #[test]
    fn edge_rebuilds_when_the_binding_access_or_floor_moved_or_the_row_is_gone() {
        let raw = b"raw-group".to_vec();
        let mut built_on = member_bound_summary("shared", &raw, Some("writer"));
        built_on.content_key_floor = Some(1);
        let basis = BindingBasis::of(&built_on);

        let mut rotated = built_on.clone();
        rotated.content_key_floor = Some(2);
        let mut demoted = built_on.clone();
        demoted.access = Some("reader".into());
        let mut rebound = built_on.clone();
        rebound.mls_group_id = Some(hex::encode(b"another-group"));
        let mut unbound = built_on.clone();
        unbound.mls_group_id = None;
        for (what, now) in [
            ("the floor advanced", Some(&rotated)),
            ("the access changed", Some(&demoted)),
            ("the group binding changed", Some(&rebound)),
            ("the set was unbound", Some(&unbound)),
            ("the row is gone (removed / deleted)", None),
        ] {
            assert_eq!(
                verdict(&basis, Some(1), now),
                EdgeVerdict::Rebuild,
                "{what} must re-resolve"
            );
        }
        // An owner-only set that was shared since the build.
        let own = summary("own");
        let shared_since = bound_summary("own", &raw);
        assert_eq!(
            verdict(&BindingBasis::of(&own), None, Some(&shared_since)),
            EdgeVerdict::Rebuild
        );
    }

    #[test]
    fn edge_keeps_re_reading_custody_while_the_floor_stays_ahead() {
        // Custody arriving is invisible on the row: a host rebuilt behind the
        // floor must re-read at every edge until the generation is there.
        let raw = b"raw-group".to_vec();
        let mut row = bound_summary("shared", &raw);
        row.content_key_floor = Some(2);
        let basis = BindingBasis::of(&row);
        assert_eq!(verdict(&basis, Some(1), Some(&row)), EdgeVerdict::Rebuild);
        assert_eq!(verdict(&basis, None, Some(&row)), EdgeVerdict::Rebuild);
    }

    #[test]
    fn binding_basis_carries_the_floor_the_row_projects() {
        let mut row = bound_summary("shared", b"g");
        assert_eq!(BindingBasis::of(&row).content_key_floor(), None);
        row.content_key_floor = Some(7);
        assert_eq!(BindingBasis::of(&row).content_key_floor(), Some(7));
    }

    #[test]
    fn binding_refuses_served_set_without_custody_or_config() {
        // Served group-less set with no resolvable keys must NOT run at all —
        // the owner path would seal under BackupKey/plaintext (FS-BIND-5/FS-5DC).
        let s = served_summary("served-docs");
        // Custody calls the set served and holds no generation for it yet
        // (the stamp joined before the keys did).
        let mut stamped = empty_cfg();
        stamped.sets.push(fauna_core::data::FolderKeyCustody {
            channel_id: Some(fauna_core::folder_keys::serve_custody_channel_id(
                "served-docs",
            )),
            served_at: Some(2_000),
            ..Default::default()
        });
        assert_eq!(
            decide(
                true,
                Some(&s),
                Some(&stamped),
                None,
                AttestationMemory::default()
            ),
            EngineContentDecision::Refuse
        );
        // Custody loaded and silent about the set: the roster's flag alone is
        // not the owner's word — the set is owner-only until the owner serves
        // it (`writer-signed-change-records.md` ruling (7)(b)(ii) rules (2)
        // and (5)).
        assert_eq!(
            decide(
                true,
                Some(&s),
                Some(&empty_cfg()),
                None,
                AttestationMemory::default()
            ),
            EngineContentDecision::Run(None, None, sealed()),
        );
        // Custody load failed entirely.
        assert_eq!(
            decide(true, Some(&s), None, None, AttestationMemory::default()),
            EngineContentDecision::Refuse
        );
    }

    // ── Phase 4 — the `public`-audience write-arm flag, owner-attested ──

    /// The seat's own identity — the trusted owner of every row it owns.
    fn seat() -> ActorKeypair {
        ActorKeypair::from_secret([7u8; 32])
    }

    /// The verdict a seat with no history reaches on a row it must not arm:
    /// sealed, and a memory a burn of nothing leaves untouched.
    fn sealed() -> DeclassificationVerdict {
        DeclassificationVerdict {
            unsealed: false,
            memory: AttestationMemory::default(),
        }
    }

    /// A genuine owner attestation over `s` (its id and name), by `signer`.
    fn attest(s: &mut FolderSummary, signer: &ActorKeypair, counter: u64) {
        s.audience = "public".to_string();
        s.audience_attestation = Some(fauna_protocol::folders::AudienceAttestation::mint(
            signer, s.id, &s.name, counter, None,
        ));
    }

    /// The verdict an ARMED seat carries: unsealed, memory raised to `counter`.
    fn armed(counter: u64) -> DeclassificationVerdict {
        DeclassificationVerdict {
            unsealed: true,
            memory: AttestationMemory {
                floor: counter,
                armed: Some(counter),
            },
        }
    }

    #[test]
    fn binding_public_owner_only_set_runs_with_the_public_flag() {
        // The owner declassified an unbound folder — and attested it: the engine
        // runs keyless-by-design with the public flag armed — the ONE principled
        // plaintext write arm (folders.md § Target re-model).
        let mut s = summary("public-site");
        attest(&mut s, &seat(), 1_000);
        assert_eq!(
            decide(
                true,
                Some(&s),
                None,
                Some(&seat().actor_id()),
                AttestationMemory::default()
            ),
            EngineContentDecision::Run(None, None, armed(1_000))
        );
    }

    /// The finding itself, at this reader: a row the nest reports
    /// `public` with no attestation, with a stranger's, or with one lent from
    /// another folder, runs **sealed** — and burns whatever the seat was armed
    /// under. A seat with no trusted owner (a member row on a host that never
    /// stamped the channel) seals a genuine one too.
    #[test]
    fn binding_public_claim_without_the_owners_attestation_stays_sealed() {
        let was_armed = AttestationMemory {
            floor: 900,
            armed: Some(900),
        };
        let burned = DeclassificationVerdict {
            unsealed: false,
            memory: was_armed.observe_sealed(),
        };

        let mut bare = summary("public-site");
        bare.audience = "public".to_string();
        assert_eq!(
            decide(true, Some(&bare), None, Some(&seat().actor_id()), was_armed),
            EngineContentDecision::Run(None, None, burned),
            "a bare claim seals and burns"
        );

        let mut forged = summary("public-site");
        attest(&mut forged, &ActorKeypair::from_secret([9u8; 32]), 1_000);
        assert_eq!(
            decide(
                true,
                Some(&forged),
                None,
                Some(&seat().actor_id()),
                was_armed
            ),
            EngineContentDecision::Run(None, None, burned),
            "a stranger's signature seals and burns"
        );

        let mut lent = summary("public-site");
        attest(&mut lent, &seat(), 1_000);
        lent.id = 301;
        assert_eq!(
            decide(true, Some(&lent), None, Some(&seat().actor_id()), was_armed),
            EngineContentDecision::Run(None, None, burned),
            "an attestation for another folder id seals and burns"
        );

        let mut genuine = summary("public-site");
        attest(&mut genuine, &seat(), 1_000);
        assert_eq!(
            decide(true, Some(&genuine), None, None, was_armed),
            EngineContentDecision::Run(None, None, burned),
            "no trusted owner ⇒ nothing to verify against ⇒ sealed"
        );
    }

    /// Replay: after the owner flips back, a successful list that no longer
    /// verifies the folder public burns the counter the seat was armed under;
    /// the nest re-serving that very attestation later finds it below the floor
    /// and arms nothing, while the owner's honest re-flip (a higher counter)
    /// arms again.
    #[test]
    fn binding_refuses_a_replayed_attestation_after_a_flip_back() {
        let mut public = summary("public-site");
        attest(&mut public, &seat(), 1_000);
        let armed_at = match decide(
            true,
            Some(&public),
            None,
            Some(&seat().actor_id()),
            AttestationMemory::default(),
        ) {
            EngineContentDecision::Run(None, None, verdict) => {
                assert!(verdict.unsealed);
                verdict.memory
            }
            other => panic!("expected an armed Run, got {other:?}"),
        };

        // The flip-back: the row is private again (the nest keeps serving the
        // stale attestation, inert on a non-public row).
        let mut private = public.clone();
        private.audience = "private".to_string();
        let burned = match decide(
            true,
            Some(&private),
            None,
            Some(&seat().actor_id()),
            armed_at,
        ) {
            EngineContentDecision::Run(None, None, verdict) => {
                assert!(!verdict.unsealed);
                verdict.memory
            }
            other => panic!("expected a sealed Run, got {other:?}"),
        };
        assert_eq!(
            burned.floor, 1_001,
            "the burn lifts the floor past the armed counter"
        );

        // The replay: the same row as before the flip-back.
        assert_eq!(
            decide(true, Some(&public), None, Some(&seat().actor_id()), burned),
            EngineContentDecision::Run(
                None,
                None,
                DeclassificationVerdict {
                    unsealed: false,
                    memory: burned
                }
            ),
            "the withdrawn attestation never re-arms this seat"
        );

        // The honest re-flip mints above what the nest last served.
        let mut reflipped = summary("public-site");
        attest(&mut reflipped, &seat(), 5_000);
        assert_eq!(
            decide(
                true,
                Some(&reflipped),
                None,
                Some(&seat().actor_id()),
                burned
            ),
            EngineContentDecision::Run(None, None, armed(5_000))
        );
    }

    /// A folder gone from a successful list is a sealed verdict that burns —
    /// the seat's armed counter goes with the row.
    #[test]
    fn binding_absent_row_seals_and_burns() {
        let was_armed = AttestationMemory {
            floor: 900,
            armed: Some(900),
        };
        assert_eq!(
            decide(true, None, None, Some(&seat().actor_id()), was_armed),
            EngineContentDecision::Run(
                None,
                None,
                DeclassificationVerdict {
                    unsealed: false,
                    memory: was_armed.observe_sealed()
                }
            )
        );
    }

    /// A **member** seat's trusted owner is the channel's MLS-recorded folder
    /// owner (`MlsEngine::folder_channel_owner`, through
    /// `config::MlsChannelOwners`) — never the owner field the nest fills on
    /// the row — and it **follows the owner's succession**: once the folder rail re-stamps the marker to the verified
    /// successor, an attestation signed by the retired key fails as a wrong
    /// signer and burns, even over a higher counter, and only the successor's
    /// arms the seat.
    #[test]
    fn binding_member_seat_follows_the_folder_owner_marker_through_a_succession() {
        let raw = b"succession-group".to_vec();
        let channel = fauna_mls::types::ChannelId::from_group_id(&raw);
        let member_engine = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let owner = ActorKeypair::from_secret([1u8; 32]);
        let successor = ActorKeypair::from_secret([2u8; 32]);
        // The folder-Welcome join's stamp.
        member_engine.mark_folder_channel_owner(&channel, &owner.actor_id());
        let owners = crate::config::MlsChannelOwners(&member_engine);
        let anchor = fauna_client_folders::DeclassificationAnchor {
            own: fauna_core::identity::ActorId([7u8; 32]),
            channel_owners: Some(&owners),
        };

        let mut row = member_bound_summary("shared", &raw, Some("writer"));
        // The nest's word about the owner — ignored by construction.
        row.owner_actor_id = Some(successor.actor_id().to_hex());
        attest(&mut row, &owner, 1_000);
        let trusted = anchor.trusted_owner_for(&row);
        assert_eq!(
            trusted,
            Some(owner.actor_id()),
            "the marker, not the nest's field"
        );
        let armed_at = match decide(
            true,
            Some(&row),
            None,
            trusted.as_ref(),
            AttestationMemory::default(),
        ) {
            EngineContentDecision::Run(Some(_), None, verdict) => {
                assert!(verdict.unsealed, "the recorded owner's attestation arms");
                verdict.memory
            }
            other => panic!("expected a bound-keyless Run, got {other:?}"),
        };

        // The succession: the folder rail re-stamps the marker to the
        // verified successor (`fauna_conversations` `route_folder_succession`).
        member_engine.mark_folder_channel_owner(&channel, &successor.actor_id());
        let trusted = anchor.trusted_owner_for(&row);
        assert_eq!(trusted, Some(successor.actor_id()));

        let mut retired_key = member_bound_summary("shared", &raw, Some("writer"));
        attest(&mut retired_key, &owner, 2_000);
        match decide(true, Some(&retired_key), None, trusted.as_ref(), armed_at) {
            EngineContentDecision::Run(Some(_), None, verdict) => {
                assert!(
                    !verdict.unsealed,
                    "the retired key arms nothing after the succession"
                );
                assert_eq!(
                    verdict.memory.floor, 1_001,
                    "…and the seat burns what it held"
                );
            }
            other => panic!("expected a sealed Run, got {other:?}"),
        }

        let mut successors = member_bound_summary("shared", &raw, Some("writer"));
        attest(&mut successors, &successor, 2_000);
        match decide(
            true,
            Some(&successors),
            None,
            trusted.as_ref(),
            armed_at.observe_sealed(),
        ) {
            EngineContentDecision::Run(Some(_), None, verdict) => {
                assert!(verdict.unsealed, "the successor's attestation arms");
            }
            other => panic!("expected an armed Run, got {other:?}"),
        }
    }

    #[test]
    fn binding_public_bound_set_keeps_its_keys_and_arms_the_flag() {
        // A declassified BOUND folder keeps its bound-marker (custody
        // unavailable here ⇒ keyless) — the keys still open the sealed
        // pre-declassify back-catalogue; only the write side goes plaintext.
        let raw = b"raw-group-id-public".to_vec();
        let mut s = bound_summary("public-shared", &raw);
        attest(&mut s, &seat(), 1_000);
        assert_eq!(
            decide(
                true,
                Some(&s),
                None,
                Some(&seat().actor_id()),
                AttestationMemory::default()
            ),
            EngineContentDecision::Run(Some(raw), None, armed(1_000))
        );
    }

    #[test]
    fn binding_non_public_audiences_never_arm_the_flag() {
        // Every other value — absent/empty (an unset audience), "private", or
        // garbage — resolves sealed: the fail-safe default.
        for audience in ["", "private", "shared", "PUBLIC", "garbage"] {
            let mut s = summary("sealed-folder");
            s.audience = audience.to_string();
            assert_eq!(
                decide(true, Some(&s), None, None, AttestationMemory::default()),
                EngineContentDecision::Run(None, None, sealed()),
                "audience {audience:?} must resolve sealed"
            );
        }
    }

    #[test]
    fn binding_public_plus_webdav_projection_stays_sealed() {
        // `webdav_enabled` + `public` is nest-refused and cannot legitimately
        // exist; a corrupt projection carrying both resolves through the
        // served (sealed) path — the fail-closed direction — which here has
        // no custody and therefore refuses outright.
        let mut s = served_summary("impossible");
        s.audience = "public".to_string();
        assert_eq!(
            decide(true, Some(&s), None, None, AttestationMemory::default()),
            EngineContentDecision::Refuse
        );
    }
}
