//! UniFFI façade for the `fauna.folders.*` WS-RPC kinds — the Devices /
//! Backups page folder control plane: the folder list (`list`), per-set
//! syncing-device list with progress (`devices`), enrolled-member roster
//! (`members.{add,list,remove}`), and
//! create / update / delete. The WS-RPC twins of the deleted
//! `/api/v1/file-sets/*` HTTP (`api-layers.md` § Folders).
//!
//! [`FfiFoldersClient`] wraps `fauna_client_folders::FoldersClient` (which
//! wraps the shared `NestClient`); the mirror records below are the FFI-visible
//! shape of the `fauna_protocol::folders::*` replies the Devices / Backups
//! pages consume. The Rust-native Linux app calls the same `FoldersClient`
//! directly — this seam gives Apple / Windows / Android the identical surface
//! over UniFFI. Construct via [`crate::nest_client::FfiNestClient::folders`].
//!
//! Mirror convention (matching `snapshots_client.rs`): only the fields the
//! pages render are mirrored; the freeform `extra` forward-compat map is
//! dropped at the boundary. Device ids cross as the hex `String` the wire
//! already carries (`fauna_protocol::folders` device ids are hex strings, not
//! raw bytes — unlike the snapshot mirrors); `retention_policy` rides as the
//! opaque JSON `String` the nest stores; counts / timestamps / intervals ride
//! as `i64`.
//!
//! The exclusive-write lease (`lease.{acquire,release}`) is intentionally NOT
//! surfaced here — no client UI consumes it (it is sync-engine
//! territory); the shared `FoldersClient` carries it for native callers.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_folders::FoldersClient;
use fauna_client_folders::folders::{FolderCreateRequest, FolderUpdateRequest};
use fauna_client_snapshots::SnapshotsClient;

use crate::{FfiError, stringify};

/// A set lifecycle helper's error across the boundary: the nest's own error
/// keeps its typed classification ([`stringify`]); a custody write failure is
/// general.
fn lifecycle_err(
    e: fauna_client_folders::SetLifecycleError<fauna_client::NestClientError>,
) -> FfiError {
    match e {
        fauna_client_folders::SetLifecycleError::Nest(e) => stringify(e),
        e @ fauna_client_folders::SetLifecycleError::Custody(_) => {
            FfiError::General { msg: e.to_string() }
        }
    }
}

// ── reply mirrors ────────────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::folders::FolderSummary`] — one folder
/// with its config and cached stat columns (the Devices/Backups folder
/// list row). `create` also returns this shape, with the cached stats at
/// `0`/`None` (a fresh set has no snapshots yet).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFolder {
    pub id: i64,
    pub name: String,
    /// Opaque JSON-string retention policy; `None` when unset.
    pub retention_policy: Option<String>,
    pub cached_snapshot_count: i64,
    pub cached_total_bytes: i64,
    pub cached_last_snapshot_at: Option<i64>,
    pub include_paths: Option<Vec<String>>,
    pub exclude_paths: Option<Vec<String>>,
    /// The caller's role for this row (`"owner"` | `"member"`); `None` if the
    /// nest's reply omits it (treat as owner — the nest always stamps it). Needed by an owner-run-only pass (S8 D3)
    /// to skip a set this connection only reads: a member's stamp is one the
    /// flip's scrub cannot attribute to the owner.
    pub role: Option<String>,
}

/// FFI mirror of [`fauna_protocol::folders::FolderDevice`] — one device
/// syncing a folder, with its sync progress.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFolderDevice {
    /// Hex-encoded 32-byte device id.
    pub device_id: String,
    pub label: String,
    pub last_change_at: i64,
    pub change_count: i64,
}

/// FFI mirror of [`fauna_protocol::folders::FolderMember`] — one enrolled
/// member of a folder, **already projected for the device-place editor**: the
/// three flags are `fauna_protocol::folders::place_rows` applied here, so the
/// four FFI apps painting `folder-place-row` never re-derive a seat in Kotlin /
/// Swift / C#.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFolderMember {
    /// Hex-encoded 32-byte device id.
    pub device_id: String,
    pub label: String,
    /// Files added on this device upload.
    pub originates: bool,
    /// Remote changes land on this device.
    pub accepts: bool,
    /// A peer's delete deletes here (OFF = an archive seat).
    pub applies_deletes: bool,
}

/// Zip a `members.list` reply with its projected rows (the ONE shared rule,
/// `fauna_protocol::folders::place_rows`, in reply order — the index is what
/// `folder-place-row[j]` addresses). The row's label wins: it is the name the
/// projection resolved, where the member's is the sealed-away blank.
pub(crate) fn ffi_folder_members(
    members: Vec<fauna_protocol::folders::FolderMember>,
    rows: Vec<fauna_protocol::folders::PlaceRow>,
) -> Vec<FfiFolderMember> {
    members
        .into_iter()
        .zip(rows)
        .map(|(m, row)| FfiFolderMember {
            device_id: m.device_id,
            label: row.label,
            originates: row.originates,
            accepts: row.accepts,
            applies_deletes: row.applies_deletes,
        })
        .collect()
}

/// FFI mirror of [`fauna_protocol::folders::FolderActorMember`] — one actor
/// (user) a shared folder is shared with, or its owner: the owner-side
/// "Shared with" list. `role` is `"owner"` | `"member"` and `handle` is empty
/// when unknown (a remote actor, or a local user with no handle set).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiFolderActorMember {
    /// Hex-encoded 32-byte actor id.
    pub actor_id: String,
    /// Display handle; empty when unknown (remote / unset).
    pub handle: String,
    /// `"owner"` | `"member"`.
    pub role: String,
    /// The pre-computed `folder-member-handle` label — render this, never
    /// re-derive: the handle when present, else the canonical `short_id` of
    /// [`Self::actor_id`] (`fauna_core::format::account_display_label`, the
    /// same rule behind `FfiPendingShare::shared_by_display` and
    /// `FolderSummary::owner_display`). Never the raw 64-hex id.
    pub display: String,
    /// `"reader"` | `"writer"` access grant (multi-writer Phase 1;
    /// `folder-member-role-select`'s value). `None` on `role == "owner"` rows
    /// and when no writer grant exists (⇒ treat as reader).
    pub access: Option<String>,
    /// Writer byte cap (`folder-member-cap-input`); `None` = uncapped.
    pub byte_cap: Option<i64>,
    /// Abuse counter of bytes this member's records currently contribute.
    pub bytes_used: Option<i64>,
    /// `true` for a **cross-nest** member (their home nest relays their reads;
    /// Phase 2, `ui/folders.md` § Sharing → Cross-nest members). Renders the
    /// remote marker on the roster row. `false` for every local member
    /// (wire-additive `Option` collapsed to the safe default).
    pub remote: bool,
}

impl From<fauna_client_folders::folders::FolderActorMember> for FfiFolderActorMember {
    fn from(m: fauna_client_folders::folders::FolderActorMember) -> Self {
        Self {
            // `handle` is empty (not absent) when unknown; `account_display_label`
            // treats an empty handle as absent and falls back to `short_id`.
            display: fauna_core::format::account_display_label(Some(&m.handle), &m.actor_id),
            actor_id: m.actor_id,
            handle: m.handle,
            role: m.role,
            access: m.access,
            byte_cap: m.byte_cap,
            bytes_used: m.bytes_used,
            remote: m.remote.unwrap_or(false),
        }
    }
}

/// The `role == "member"` subset of a folder actor roster (the owner
/// excluded) — the owner-side "Shared with" list, and the source of the
/// `folder-shared-badge` "Shared · N" count (`.len()`/`.count`/`.size` of the
/// result on the calling side — no separate count export). Mirrors
/// `fauna_client_folders::member_actors` for UniFFI callers; the single
/// derivation site apple/android/windows filter `members_list_actors` through
/// before rendering, replacing 3 independent local filters.
/// The reconcile backstop's cadence, in seconds —
/// `fauna_client_folders::DEFAULT_RESCAN_INTERVAL` (300 s), the ONE value every
/// seat ticks at since phase 5 of the folders re-model retired the per-folder
/// choice (`file-sync.md` § Config, the phase-5 block). For the native shells
/// that schedule a tick themselves without an engine host to ask — the apple
/// File Provider extension's re-pull tick, android's documents provider.
/// Crossing it once keeps every shell off its own copy of the number.
#[uniffi::export]
pub fn default_rescan_interval_secs() -> u64 {
    fauna_client_folders::DEFAULT_RESCAN_INTERVAL.as_secs()
}

#[uniffi::export]
pub fn folder_member_actors(actors: Vec<FfiFolderActorMember>) -> Vec<FfiFolderActorMember> {
    actors.into_iter().filter(|m| m.role == "member").collect()
}

/// Resolve a folder row's `FolderRef` wire string — `local:<id>` for any
/// row with a row on this holder's own nest (owner or same-nest member),
/// `foreign:<64-hex>` for a cross-nest row (`home_nest_url` present); `None`
/// when neither arm resolves (a malformed `mls_group_id_hex` on a foreign
/// row), which every caller answers by **refusing the bind** — a folder
/// binding is keyed by its ref alone. The single owner of "which arm" so apps
/// cannot drift on it — call this, never re-derive the choice
/// (`fauna_client_folders::engine_binding::folder_ref_for_row`,
/// `on-demand-files.md` § Hosting multiple on-demand folders — *A location
/// binding identifies its set by a `FolderRef`, not by name*).
///
/// `folders-author`-gated (not the module's own default unconditional
/// posture): `engine_binding` itself lives behind `fauna-client-folders`'s
/// `mls` feature, which only `folders-author` turns on — default-on for
/// every UniFFI app but dropped by the Go mail-bridge's
/// `--no-default-features` build, same reason as `connection_state_label` in
/// `nest_client.rs`.
#[cfg(feature = "folders-author")]
#[uniffi::export]
pub fn folder_ref_for_row(
    id: i64,
    mls_group_id_hex: Option<String>,
    home_nest_url: Option<String>,
) -> Option<String> {
    fauna_client_folders::engine_binding::folder_ref_for_row(
        id,
        mls_group_id_hex.as_deref(),
        home_nest_url.as_deref(),
    )
    .map(|r| r.to_wire())
}

/// Whether `wire` is a `FolderRef` wire string the shared per-set seams accept
/// (`FolderRef::parse` succeeds) — the one owner of that grammar, so a shell
/// that must tell a ref from any other string (today only the apple File
/// Provider test CLI) asks here instead of re-spelling `local:` / `foreign:` itself.
/// `folders-author`-gated for the same reason as [`folder_ref_for_row`].
#[cfg(feature = "folders-author")]
#[uniffi::export]
pub fn folder_ref_is_valid(wire: String) -> bool {
    fauna_core::folder_keys::FolderRef::parse(&wire).is_some()
}

/// A set's identity on one device for one account
/// (`fauna_core::folder_keys::ActorScopedFolderRef`), every spelling a
/// per-device registry keyed by it needs: the identifier itself, the account
/// as lowercase hex, the set as its bare `FolderRef` wire string (what the
/// Rust host and drain seams take), and the identifier's ref half — the one
/// percent-encoded component the staging root is keyed by (the apple File
/// Provider domain identifier, `on-demand-files.md` § Apple File Provider
/// binding, *the actor-scoped device identity*).
#[cfg(feature = "folders-author")]
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiActorScopedFolderRef {
    /// `<folder_component>@<actor_id_hex>` — `ActorScopedFolderRef::to_wire`.
    pub scoped_id: String,
    pub actor_id_hex: String,
    /// The bare `FolderRef` wire string (`local:1`).
    pub folder_id: String,
    /// `FolderRef::path_component` (`local%3A1`): the identifier's ref half,
    /// and the single directory component every root keyed by the set uses.
    pub folder_component: String,
}

#[cfg(feature = "folders-author")]
impl From<fauna_core::folder_keys::ActorScopedFolderRef> for FfiActorScopedFolderRef {
    fn from(scoped: fauna_core::folder_keys::ActorScopedFolderRef) -> Self {
        Self {
            scoped_id: scoped.to_wire(),
            actor_id_hex: hex::encode(scoped.actor_id),
            folder_id: scoped.folder.to_wire(),
            folder_component: scoped.folder.path_component(),
        }
    }
}

/// Scope a set's `FolderRef` wire string to an account — the identity with
/// its identifier rendered (`ActorScopedFolderRef::to_wire`). `None` when
/// either half is malformed (a non-ref `folder_id`, an actor that is not 32
/// bytes of hex), so a shell never registers a device presence under an
/// identifier it cannot parse back. The grammar has one home
/// (`ActorScopedFolderRef`); the apps only ever call these two functions and
/// never spell the `@` — or the ref's encoding — themselves.
#[cfg(feature = "folders-author")]
#[uniffi::export]
pub fn actor_scoped_folder_ref(
    actor_id_hex: String,
    folder_id: String,
) -> Option<FfiActorScopedFolderRef> {
    let folder = fauna_core::folder_keys::FolderRef::parse(&folder_id)?;
    let raw = hex::decode(&actor_id_hex).ok()?;
    let actor_id = <[u8; 32]>::try_from(raw.as_slice()).ok()?;
    Some(folder.scoped_to(actor_id).into())
}

/// Parse an actor-scoped identifier back into its halves; `None` for a bare
/// ref, a set name, the pre-2026-09-29 spelling with the bare `:` in the ref
/// half, or a malformed actor (`ActorScopedFolderRef::parse`) — the answer
/// that tells a shell "this identifier scopes no set to any account: never
/// serve or key anything by it; removing it is all that is left".
#[cfg(feature = "folders-author")]
#[uniffi::export]
pub fn actor_scoped_folder_ref_parse(wire: String) -> Option<FfiActorScopedFolderRef> {
    let scoped = fauna_core::folder_keys::ActorScopedFolderRef::parse(&wire)?;
    Some(scoped.into())
}

// ── FfiFoldersClient ──────────────────────────────────────────────────────

/// UniFFI handle for the `fauna.folders.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::folders`]; methods are exposed to
/// Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiFoldersClient {
    nest: Arc<NestClient>,
}

impl FfiFoldersClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    /// The typed call surface, with this connection's **label custody** wired —
    /// a REAL resolver, not owner-only.
    ///
    /// ⚠ **The resolver is load-bearing, and owner-only custody here would be a
    /// silent defect** — which caught exactly that shape on
    /// the sibling `FfiSnapshotsClient`. `LabelCustody::owner_only` does NOT fail
    /// closed on a bound set the way one might expect: its `keys_for` no-resolver
    /// arm yields `FileDownloadKeys::owner(..)` with `mls_group_id: None`, so
    /// `label_seal_root`'s bound-set bail is skipped and the label seals under the
    /// **owner** root. For a label-audience field that is the narrowing this slice
    /// exists to avoid: at the flip the plaintext scrubs, the sealed sibling
    /// exists (so S8 skips the row), and every roster member loses the field.
    ///
    /// With the resolver wired, a bound set resolves its M2 content keys and seals
    /// under the generation its roster can open, exactly as linux already does
    /// (`apps/fauna-linux/src/client.rs::label_custody` — same
    /// `BackupKey::derive(secret)`, so no cross-app root divergence). An unbound
    /// set still takes the owner arm, which is correct there: the owner is the
    /// whole audience.
    ///
    /// `None` keypair (a bearer-only connection) ⇒ keyless custody, which seals
    /// exactly as this crate did before sealing: not at all.
    /// The account's folder-key custody the create and delete helpers write —
    /// refused on a bearer-only connection, which owns no set.
    fn custody(&self) -> Result<Arc<dyn fauna_client_folders::FolderKeyStore>, FfiError> {
        if self.nest.auth().keypair().is_none() {
            return Err(FfiError::General {
                msg: "creating or deleting a folder needs the owner's identity".into(),
            });
        }
        // Folder-key custody rests on the account plane alone, so a build
        // without the account runtime (the Go mail bridge's
        // `--no-default-features` flavor, which exports this face but never
        // owns a set) holds none and refuses rather than failing to compile.
        #[cfg(feature = "account-runtime")]
        {
            Ok(crate::account_runtime::folder_key_store())
        }
        #[cfg(not(feature = "account-runtime"))]
        {
            Err(FfiError::General {
                msg: "creating or deleting a folder needs the account runtime, which this build \
                      does not carry"
                    .into(),
            })
        }
    }

    pub(crate) fn client(&self) -> FoldersClient<Arc<NestClient>> {
        let client = FoldersClient::new(Arc::clone(&self.nest));
        let Some(_keypair) = self.nest.auth().keypair() else {
            return client;
        };
        // The audience attestor rides on every target, ungated: a `→public`
        // `set_audience` through this face must carry the owner's signature
        // (`encryption-at-rest.md` § Readable classes → *The declassification
        // is owner-ATTESTED*), or no verifying seat unseals the folder.
        let client = client.with_audience_attestor(Arc::new(
            fauna_core::identity::ActorKeypair::from_secret(*_keypair.secret_bytes()),
        ));
        // The resolver lives behind `fauna-client-folders/mls`, which this crate
        // pulls in via its own `folders-author` feature (it reads the roster to
        // learn a set's bound-ness). NB the gate must name THIS crate's feature —
        // `feature = "mls"` compiles to a permanently-false cfg here, silently
        // disabling the resolver in every profile including the default one.
        // Without it there is no way to reach a bound set's
        // M2 generation, so this build seals NOTHING rather than falling back to
        // the owner root — the trap is exactly that the owner-root fallback
        // looks like a graceful degrade and is really silent member-side loss. Not
        // sealing leaves an honest S8 backfill row; sealing wrongly does not.
        #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
        let client = {
            let resolver: Arc<dyn fauna_core::folder_keys::FolderKeyResolver> =
                Arc::new(fauna_client_folders::NestFolderKeyResolver::new(
                    Arc::clone(&self.nest),
                    crate::account_runtime::folder_key_store(),
                ));
            client.with_label_custody(fauna_core::label_custody::LabelCustody::new(
                Some(resolver),
                Some(fauna_core::crypto::BackupKey::derive(
                    _keypair.secret_bytes(),
                )),
            ))
        };
        client
    }
}

fn map_summary(s: fauna_client_folders::folders::FolderSummary) -> FfiFolder {
    FfiFolder {
        id: s.id,
        name: s.name,
        retention_policy: s.retention_policy,
        cached_snapshot_count: s.cached_snapshot_count,
        cached_total_bytes: s.cached_total_bytes,
        cached_last_snapshot_at: s.cached_last_snapshot_at,
        include_paths: s.include_paths,
        exclude_paths: s.exclude_paths,
        role: s.role,
    }
}

/// Every set the account holds, as the shared presence plan takes it — the
/// body of both presence doors ([`FfiFoldersClient::presence_sets`] in the
/// app process, `capability_host_presence_sets` beside the capability hosts),
/// which differ only in whose custody they read. The member-visible folder
/// list, this device's place on each OWN folder (never asked for a member
/// row: the roster read resolves a folder by name among the caller's own),
/// then [`fauna_folders_machine::on_demand_presence::held_sets`]'s rules.
/// Any failed read is an error, never an empty answer.
#[cfg(feature = "folders")]
pub(crate) async fn presence_sets_over<R>(
    client: &FoldersClient<Arc<NestClient>>,
    device_id_hex: &str,
    custody: &R,
) -> Result<Vec<fauna_folders_machine::on_demand_presence::PresenceSet>, FfiError>
where
    R: fauna_client_folders::FolderKeyReader + ?Sized,
{
    let rows = client
        .list_owned_and_shared()
        .await
        .map_err(stringify)?
        .folders;
    let mut own_place_accepts = std::collections::BTreeMap::new();
    for row in rows.iter().filter(|r| r.role.as_deref() != Some("member")) {
        let roster = client
            .members_list(row.name.clone())
            .await
            .map_err(stringify)?;
        own_place_accepts.insert(
            row.id,
            fauna_protocol::folders::SeatRead::find(&roster.members, device_id_hex)
                .delivers_presence(),
        );
    }
    let custody = custody.load().await.map_err(|e| FfiError::General {
        msg: format!("folder-key custody unreadable: {e:#}"),
    })?;
    Ok(fauna_folders_machine::on_demand_presence::held_sets(
        &rows,
        &custody,
        &own_place_accepts,
    ))
}

#[cfg(all(feature = "folders", feature = "account-runtime"))]
#[fauna_uniffi_async::export]
impl FfiFoldersClient {
    /// Every set the account holds, as the shared presence plan takes it —
    /// own folders and the folders shared with it, on this nest or another
    /// (`on-demand-files.md` § Shared sets on a capability host, decision 3),
    /// read in the APP process over this seat's own folder-key custody. The
    /// twin of `capability_host_presence_sets`, which reads the capability
    /// hosts' replica; the mapping is one ([`presence_sets_over`]). `device_id`
    /// is this device's hex id. An error is never an empty answer: the caller
    /// skips the reconcile rather than tearing presences down.
    pub async fn presence_sets(
        &self,
        device_id: String,
    ) -> Result<Vec<fauna_folders_machine::on_demand_presence::PresenceSet>, FfiError> {
        presence_sets_over(
            &self.client(),
            &device_id.to_ascii_lowercase(),
            &*crate::account_runtime::folder_key_store(),
        )
        .await
    }
}

#[fauna_uniffi_async::export]
impl FfiFoldersClient {
    /// `fauna.folders.list` — every folder the bearer actor owns, with the
    /// cached stat columns and selective-sync include/exclude paths.
    pub async fn list(&self) -> Result<Vec<FfiFolder>, FfiError> {
        let reply = self.client().list().await.map_err(stringify)?;
        Ok(reply.folders.into_iter().map(map_summary).collect())
    }

    // `rescan_interval_secs_for` used to sit here — retired with phase 5's
    // de-knob (`file-sync.md` § Config, the phase-5 block): the reconcile /
    // photo-backup cadence is the hard-coded `DEFAULT_RESCAN_INTERVAL` (300 s)
    // on every platform, never a row read, so there is no per-folder value for
    // a client to ask for. The constant itself crosses once, as the free fn
    // [`default_rescan_interval_secs`].

    /// `fauna.folders.create` — create a new folder owned by the bearer
    /// actor, sealed from birth (`create_set`). Returns the created set (cached
    /// stats `0`/`None`). A duplicate name surfaces as `fauna.folders.conflict`.
    /// There is no path list: include/exclude paths seal under the row id this
    /// create mints, so they are set by a later [`Self::update`].
    pub async fn create(
        &self,
        name: String,
        retention_policy: Option<String>,
    ) -> Result<FfiFolder, FfiError> {
        let r = fauna_client_folders::create_set(
            &self.client(),
            &*self.custody()?,
            FolderCreateRequest {
                name,
                retention_policy,
                ..Default::default()
            },
        )
        .await
        .map_err(lifecycle_err)?;
        Ok(FfiFolder {
            id: r.id,
            name: r.name,
            retention_policy: r.retention_policy,
            cached_snapshot_count: 0,
            cached_total_bytes: 0,
            cached_last_snapshot_at: None,
            include_paths: None,
            exclude_paths: None,
            role: Some("owner".into()),
        })
    }

    /// `fauna.folders.update` — partial update; every `None` field is left
    /// unchanged server-side. Pass only `include_paths`/`exclude_paths` for a
    /// selective-sync save, or only `retention_policy` for a retention edit.
    pub async fn update(
        &self,
        name: String,
        retention_policy: Option<String>,
        include_paths: Option<Vec<String>>,
        exclude_paths: Option<Vec<String>>,
    ) -> Result<(), FfiError> {
        self.client()
            .update(FolderUpdateRequest {
                name,
                retention_policy,
                include_paths,
                exclude_paths,
                // Struct-update for the "leave unchanged" tail (the repo's
                // fixture-shape convention) — this FFI surface deliberately
                // exposes only the three fields the apps edit.
                ..Default::default()
            })
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// The write behind `folder-audience-select` — set a folder's audience
    /// (`ui/folders.md` § Audience and website serving).
    ///
    /// **Keyless**: a plain `fauna.folders.update` write needing no MLS engine
    /// and no content key, because the back-catalogue is moved by each device's
    /// own engine at its next catch-up off the projected audience
    /// (`SyncEngine::converge_corpus_to_audience`), not by this caller.
    ///
    /// The door for **every** direction the picker sends — `"public"`,
    /// `"private"`, and `"shared"` on a bound folder exiting its public window
    /// (the flip-back, which `audience_options` offers exactly there).
    /// All of them converge off the projected audience alone
    /// (`SyncEngine::converge_corpus_to_audience` dispatches the sealed
    /// direction on bound-ness), on every seat, members' included — no custody
    /// sentinel, which is per-actor and could never reach a member's engine.
    ///
    /// `→public` is the confirm-gated one (`folder-audience-public-confirm`): a
    /// public folder rests **unsealed**, names and paths included. Arm the
    /// confirm in the app and call this only once it is answered.
    pub async fn set_audience(&self, name: String, audience: String) -> Result<(), FfiError> {
        self.client()
            .set_audience(&name, &audience)
            .await
            .map_err(stringify)
    }

    /// The write behind `folder-website-toggle` — publish (or stop publishing)
    /// this folder's head as the actor's website.
    ///
    /// **The only door to a website folder**: phase 2 slice e retired the create
    /// wizard's mode step, so this toggle is what restores website-folder
    /// creation — do not patch that gap by re-adding a mode control.
    ///
    /// Orthogonal to the audience, which decides who may *read* what is
    /// published; enabling it on a folder that is neither `public` nor paywalled
    /// is allowed and inert, and the app says so through
    /// [`crate::folders::website_serve_hint`] rather than by disabling the
    /// control.
    pub async fn set_website_enabled(&self, name: String, enabled: bool) -> Result<(), FfiError> {
        self.client()
            .set_website_enabled(&name, enabled)
            .await
            .map_err(stringify)
    }

    /// `fauna.folders.places.set` — the write behind the expanded owner row's
    /// place editor (`folder-place-row` + its three checkboxes).
    ///
    /// Sets what one device place *does*. **The point applies whole**, so all
    /// three flags ride every call: pass the seat's full triple, never just the
    /// box that moved. Every flag point is writable and rests unrounded.
    ///
    /// Takes the three bools rather than a record because `PlaceFlags` lives in
    /// `fauna-protocol`, which carries no UniFFI dependency — the plain-parameter
    /// idiom `members_set_access` already uses.
    ///
    /// ⚠ The caller repaints from a roster RE-READ ([`Self::members_list`]),
    /// never from an optimistic local flip: nest truth is the only thing that
    /// can report what the point came to rest as.
    pub async fn places_set(
        &self,
        name: String,
        device_id: String,
        originates: bool,
        accepts: bool,
        applies_deletes: bool,
    ) -> Result<(), FfiError> {
        self.client()
            .places_set(fauna_protocol::folders::PlacesSetRequest {
                name,
                device_id,
                flags: fauna_protocol::folders::PlaceFlags {
                    originates,
                    accepts,
                    applies_deletes,
                    ..Default::default()
                },
                ..Default::default()
            })
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// The shared enrol for a gesture that gives this device a local presence
    /// on `name` — apple's and android's `folder-on-demand-toggle` turned ON
    /// (`on-demand-files.md`, *Auto-appear default-ON*; the desktop bind runs
    /// the same helper agent-side). Writes this device's place at the default
    /// point iff it holds none; never rewrites an existing place
    /// (`fauna_client_folders::FoldersClient::ensure_place`). Returns `true`
    /// when it enrolled, `false` when the device already held a place —
    /// repaint from a roster re-read either way.
    pub async fn ensure_place(&self, name: String, device_id: String) -> Result<bool, FfiError> {
        let outcome = self
            .client()
            .ensure_place(name, &device_id)
            .await
            .map_err(stringify)?;
        Ok(outcome == fauna_client_folders::EnsurePlaceOutcome::Enrolled)
    }

    /// Whether `device_id`'s place on `name` makes it a delivery seat for an
    /// on-demand presence — the `this_device_accepts` input of the shared
    /// presence plan (`on-demand-files.md` § Apple File Provider binding:
    /// desired = own folders where this device's place has `accepts: true`).
    /// One `members.list` read through the one sanctioned seat rule
    /// (`SeatRead::delivers_presence`): no place, an unreadable place, or a
    /// place with `accepts` off all answer `false`. A failed read is an
    /// error, never `false` — a caller must not tear down a presence on a
    /// roster it could not see.
    pub async fn device_accepts(&self, name: String, device_id: String) -> Result<bool, FfiError> {
        let reply = self.client().members_list(name).await.map_err(stringify)?;
        Ok(
            fauna_protocol::folders::SeatRead::find(
                &reply.members,
                &device_id.to_ascii_lowercase(),
            )
            .delivers_presence(),
        )
    }

    /// `fauna.folders.delete` — remove a folder the bearer owns.
    /// Non-cascading: a set that still has snapshots surfaces as
    /// `fauna.folders.internal`; the caller surfaces that to the user.
    ///
    /// Retires the set's custody first — the shared delete helper
    /// (`fauna_client_folders::delete_set`).
    pub async fn delete(&self, name: String) -> Result<(), FfiError> {
        fauna_client_folders::delete_set(&self.client(), &*self.custody()?, &name)
            .await
            .map_err(lifecycle_err)?;
        Ok(())
    }

    /// `fauna.folders.devices` — the devices syncing one folder, each with
    /// its recorded sync progress (the Devices-page per-set device list).
    pub async fn devices(&self, name: String) -> Result<Vec<FfiFolderDevice>, FfiError> {
        let reply = self.client().devices(name).await.map_err(stringify)?;
        Ok(reply
            .devices
            .into_iter()
            .map(|d| FfiFolderDevice {
                device_id: d.device_id,
                label: d.label,
                last_change_at: d.last_change_at,
                change_count: d.change_count,
            })
            .collect())
    }

    // NOTE (2026-07-15 dark-rail audit): the thin `members_add` /
    // `members_remove` device-enrollment wrappers were deleted — every app
    // enrolls devices through the shared `fauna-folders-machine` wizard
    // (`fauna.folders.places.set` via the machine's nest API), and nothing
    // consumes device un-enrollment yet. Wire a future un-enroll affordance
    // through the machine, not a raw FFI wrapper.

    /// `fauna.folders.members.list` — the enrolled-device roster for one file
    /// set (`device_id`, `label`, flags), each label exactly as the nest sent
    /// it.
    ///
    /// ⚠ **Not the place editor's read.** Every user-chosen device label rests
    /// sealed, so this label is empty for a named device; an app painting
    /// `folder-place-row` calls `place_rows` (`src/devices.rs`, which takes the
    /// Devices page's roster to name the seats) instead.
    pub async fn members_list(&self, name: String) -> Result<Vec<FfiFolderMember>, FfiError> {
        let reply = self.client().members_list(name).await.map_err(stringify)?;
        let rows = fauna_protocol::folders::place_rows(&reply.members, std::iter::empty());
        Ok(ffi_folder_members(reply.members, rows))
    }

    /// `fauna.folders.members.list_actors` — the *actor* (user) roster of a
    /// shared folder: who the set is shared with (the owner-side "Shared with"
    /// list). Each entry carries the actor's hex id, its nest-resolved `handle`
    /// (empty when unknown), and a `role` of `"owner"` | `"member"`. Distinct
    /// from `members_list`, which is the enrolled *device* roster.
    pub async fn members_list_actors(
        &self,
        name: String,
    ) -> Result<Vec<FfiFolderActorMember>, FfiError> {
        let reply = self
            .client()
            .actor_members_list(name)
            .await
            .map_err(stringify)?;
        Ok(reply
            .members
            .into_iter()
            .map(FfiFolderActorMember::from)
            .collect())
    }

    /// `fauna.folders.members.set_access` — the **owner** grants or edits a
    /// member's `"reader"`/`"writer"` access (+ optional byte cap, `None` =
    /// uncapped) on their shared set (multi-writer Phase 1; the write behind
    /// `folder-member-role-select` / `folder-member-cap-input`). Role
    /// transitions never rotate the content key — removal is `members.evict`.
    pub async fn members_set_access(
        &self,
        name: String,
        actor_id: String,
        access: String,
        byte_cap: Option<i64>,
    ) -> Result<(), FfiError> {
        self.client()
            .members_set_access(fauna_protocol::folders::MemberSetAccessRequest {
                name,
                actor_id,
                access,
                byte_cap,
                ..Default::default()
            })
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// The S8 D1 client-driven seal backfill (file-sync.md § Sealed names &
    /// paths → Implementation status today): stamp every owned set whose
    /// folder-plane sealed sibling is still missing, from the still-resting
    /// dual-write plaintext. Call once per app session start on a
    /// custody-wired client (`self.client()` already wires the resolver +
    /// owner `BackupKey` — no second derivation here, per the S8 rule). A
    /// bound row whose M2 keys this custody cannot resolve is skipped
    /// fail-closed (`bound_skipped`); best-effort, an unreachable nest just
    /// retries next session.
    pub async fn backfill_sealed_fields(&self) -> Result<FfiSealBackfillReport, FfiError> {
        let report = self
            .client()
            .backfill_sealed_fields()
            .await
            .map_err(stringify)?;
        Ok(FfiSealBackfillReport {
            names: report.names as u32,
            selective_sync: report.selective_sync as u32,
            retention: report.retention as u32,
            bound_skipped: report.bound_skipped as u32,
            identity_mismatch: report.identity_mismatch as u32,
            update_failures: report.update_failures as u32,
        })
    }

    /// The whole S8 seal-backfill **sweep** — D1 ([`Self::backfill_sealed_fields`])
    /// then D3 per owned set ([`FfiSnapshotsClient::backfill_tag_seals`][snap]),
    /// skipping `role == "member"` rows (a member's stamp is one
    /// the S9 flip's scrub cannot attribute to the owner). This is the ONE
    /// sequencing seam every UniFFI app now calls at its post-auth hook instead
    /// of hand-rolling the D1-then-D3-skip-member loop itself (
    /// `docs/goal/behavior/path-sealing.md` § Implementation status today),
    /// driving the shared `seal_backfill` module. Best-effort throughout; never fails — read
    /// the report only to log (never a set name — S7).
    ///
    /// [snap]: crate::snapshots_client::FfiSnapshotsClient::backfill_tag_seals
    pub async fn run_seal_backfill_sweep(&self) -> FfiSealBackfillSweepReport {
        #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
        let report = match self.nest.auth().keypair() {
            Some(keypair) => {
                let keypair =
                    fauna_core::identity::ActorKeypair::from_secret(*keypair.secret_bytes());
                fauna_client_folders::seal_backfill::run_sweep(
                    Arc::clone(&self.nest),
                    &keypair,
                    crate::account_runtime::folder_key_store(),
                )
                .await
            }
            None => {
                fauna_client_folders::seal_backfill::sweep_with(
                    &self.client(),
                    &SnapshotsClient::new(Arc::clone(&self.nest)),
                )
                .await
            }
        };
        #[cfg(not(all(feature = "folders-author", not(target_arch = "wasm32"))))]
        let report = fauna_client_folders::seal_backfill::sweep_with(
            &self.client(),
            &SnapshotsClient::new(Arc::clone(&self.nest)),
        )
        .await;

        FfiSealBackfillSweepReport::from(report)
    }
}

/// FFI mirror of [`fauna_client_folders::SealBackfillReport`] (S8 D1). All-zero
/// = converged, the steady state after one pass.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSealBackfillReport {
    pub names: u32,
    pub selective_sync: u32,
    pub retention: u32,
    pub bound_skipped: u32,
    pub identity_mismatch: u32,
    pub update_failures: u32,
}

/// FFI mirror of [`fauna_client_folders::seal_backfill::SealBackfillSweepReport`]
/// (S8 D1+D3, one [`FfiFoldersClient::run_seal_backfill_sweep`] pass).
/// All-zero/`None`-free = converged, the steady state after the first pass on a
/// given nest.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiSealBackfillSweepReport {
    /// The D1 folder-plane pass, or `None` when it could not run at all (see
    /// [`Self::fields_error`]).
    pub fields: Option<FfiSealBackfillReport>,
    /// Why the D1 pass could not run (nest unreachable at session start is the
    /// ordinary case).
    pub fields_error: Option<String>,
    /// The D3 snapshot-tag pass, summed over every set swept.
    pub tags: crate::snapshots_client::FfiTagSealBackfillReport,
    /// Owned sets the D3 pass visited.
    pub sets_swept: u32,
    /// Sets skipped because this actor is a *member*, not the owner.
    pub member_sets_skipped: u32,
    /// Sets whose D3 call failed outright (transport fault) — skipped, the
    /// sweep reruns at the next start.
    pub set_failures: u32,
    /// Why the roster read before the D3 loop failed, when it did (no set was
    /// swept in that case).
    pub roster_error: Option<String>,
}

impl From<fauna_client_folders::seal_backfill::SealBackfillSweepReport>
    for FfiSealBackfillSweepReport
{
    fn from(r: fauna_client_folders::seal_backfill::SealBackfillSweepReport) -> Self {
        Self {
            fields: r.fields.map(|f| FfiSealBackfillReport {
                names: f.names as u32,
                selective_sync: f.selective_sync as u32,
                retention: f.retention as u32,
                bound_skipped: f.bound_skipped as u32,
                identity_mismatch: f.identity_mismatch as u32,
                update_failures: f.update_failures as u32,
            }),
            fields_error: r.fields_error,
            tags: crate::snapshots_client::FfiTagSealBackfillReport {
                stamped: r.tags.stamped as u32,
                unsealable: r.tags.unsealable as u32,
                stamp_failures: r.tags.stamp_failures as u32,
            },
            sets_swept: r.sets_swept as u32,
            member_sets_skipped: r.member_sets_skipped as u32,
            set_failures: r.set_failures as u32,
            roster_error: r.roster_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The regression pin — driven from the FAÇADE, not a replica of
    /// its custody.** The fauna-core pin characterizes
    /// `LabelCustody::owner_only` but structurally cannot observe THIS crate
    /// reverting to it (fauna-core cannot depend on fauna-ffi) — proven by reverting `client()` with every test staying green. This pin
    /// closes that: it builds the façade over an offline `NestClient` (no
    /// connection is made — `client()` only constructs) and asserts the custody
    /// the typed client will actually seal with carries a resolver.
    ///
    /// Mutation: revert `client()` to `LabelCustody::owner_only(..)` →
    /// `has_resolver()` is false → exactly this pin reds
    /// (`cargo test -p fauna-ffi --lib`, same crate as the production change).
    /// The real resolver's *semantics* stay covered where they are testable:
    /// `NestFolderKeyResolver::resolve` is an RPC read needing a live nest, so
    /// the custody-shape consequences are pinned in fauna-core
    /// (`owner_only_custody_seals_a_bound_set_where_a_member_cannot_follow`)
    /// and the resolver itself by linux's integration coverage.
    #[cfg(all(feature = "folders-author", not(target_arch = "wasm32")))]
    #[test]
    fn the_facade_hands_its_client_a_resolver_wired_custody() {
        let nest = NestClient::new(
            "wss://unreachable.invalid".into(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        );
        let facade = FfiFoldersClient::from_nest(nest);
        let client = facade.client();
        let custody = client.label_custody();
        let custody_shape = (custody.has_resolver(), custody.has_owner_key());
        assert_eq!(
            custody_shape,
            (true, true),
            "the retention seal funnel needs BOTH arms: the resolver so a bound \
             set seals under the M2 generation its roster can open (\
             owner-only custody here silently seals where no member can follow), \
             and the owner key so an unbound set still seals and renders (the \
             positive-control arm)"
        );
    }

    /// Every field of the shared sweep report crosses the FFI boundary
    /// unrenamed and unswapped.
    ///
    /// Mutation: swap any two field assignments in
    /// `From<SealBackfillSweepReport> for FfiSealBackfillSweepReport` (e.g.
    /// `sets_swept`/`member_sets_skipped`) → the corresponding pair of
    /// distinct-valued asserts reds.
    #[test]
    fn seal_backfill_sweep_report_mirrors_every_field() {
        let r = fauna_client_folders::seal_backfill::SealBackfillSweepReport {
            fields: Some(fauna_client_folders::SealBackfillReport {
                names: 1,
                selective_sync: 2,
                retention: 3,
                bound_skipped: 4,
                identity_mismatch: 5,
                update_failures: 6,
            }),
            fields_error: Some("fields boom".into()),
            tags: fauna_client_snapshots::TagSealBackfillReport {
                stamped: 7,
                unsealable: 8,
                stamp_failures: 9,
            },
            sets_swept: 10,
            member_sets_skipped: 11,
            set_failures: 12,
            roster_error: Some("roster boom".into()),
        };

        let mirrored = FfiSealBackfillSweepReport::from(r);

        let fields = mirrored.fields.expect("fields present");
        assert_eq!(fields.names, 1);
        assert_eq!(fields.selective_sync, 2);
        assert_eq!(fields.retention, 3);
        assert_eq!(fields.bound_skipped, 4);
        assert_eq!(fields.identity_mismatch, 5);
        assert_eq!(fields.update_failures, 6);
        assert_eq!(mirrored.fields_error.as_deref(), Some("fields boom"));
        assert_eq!(mirrored.tags.stamped, 7);
        assert_eq!(mirrored.tags.unsealable, 8);
        assert_eq!(mirrored.tags.stamp_failures, 9);
        assert_eq!(mirrored.sets_swept, 10);
        assert_eq!(mirrored.member_sets_skipped, 11);
        assert_eq!(mirrored.set_failures, 12);
        assert_eq!(mirrored.roster_error.as_deref(), Some("roster boom"));
    }

    #[test]
    fn folder_mirror_carries_config_and_cached_stats() {
        let fs = FfiFolder {
            id: 3,
            name: "photos".into(),
            retention_policy: Some("{\"keep_last\":5}".into()),
            cached_snapshot_count: 7,
            cached_total_bytes: 4096,
            cached_last_snapshot_at: Some(1_700_000_000),
            include_paths: Some(vec!["/home/me/pics".into()]),
            exclude_paths: None,
            role: Some("owner".into()),
        };
        assert_eq!(fs.name, "photos");
        assert_eq!(fs.cached_snapshot_count, 7);
        assert_eq!(
            fs.include_paths.as_deref(),
            Some(&["/home/me/pics".into()][..])
        );
    }

    #[test]
    fn map_summary_preserves_fields() {
        let s = fauna_client_folders::folders::FolderSummary {
            id: 1,
            name: "docs".into(),
            retention_policy: None,
            conflict_policy: None,
            cached_snapshot_count: 2,
            cached_total_bytes: 1024,
            cached_last_snapshot_at: None,
            include_paths: None,
            exclude_paths: Some(vec!["/tmp".into()]),
            mls_group_id: None,
            role: Some("owner".into()),
            access: None,
            owner_handle: None,
            owner_actor_id: None,
            webdav_enabled: false,
            web_paywall_tier: None,
            // Struct-update for the sealed-label tail (the repo's fixture-shape
            // convention) — this test's subject is field mapping.
            ..Default::default()
        };
        let m = map_summary(s);
        assert_eq!(m.id, 1);
        assert_eq!(m.exclude_paths.as_deref(), Some(&["/tmp".into()][..]));
    }

    /// The `folder-member-handle` label is precomputed through the canonical
    /// `account_display_label` rule (`docs/goal/behavior/value-formatting.md`
    /// § Account display label) — the handle when present, else the canonical
    /// `short_id` of the actor hex. The raw 64-hex id is NEVER the label: the
    /// hand-rolled `handle.is_empty() ? actor_id : handle` fallback this
    /// replaces rendered a full 64-character blob into the roster row.
    #[test]
    fn from_wire_computes_member_display_fallback() {
        let actor = "a".repeat(64);

        let with_handle =
            FfiFolderActorMember::from(fauna_client_folders::folders::FolderActorMember {
                actor_id: actor.clone(),
                handle: "alice".into(),
                role: "member".into(),
                ..Default::default()
            });
        assert_eq!(with_handle.display, "alice");

        // Handle empty — a remote actor, or a local user with no handle set.
        let handle_less =
            FfiFolderActorMember::from(fauna_client_folders::folders::FolderActorMember {
                actor_id: actor.clone(),
                handle: String::new(),
                role: "member".into(),
                ..Default::default()
            });
        assert_eq!(handle_less.display, fauna_core::format::short_id(&actor));
        assert_ne!(
            handle_less.display, actor,
            "the raw 64-hex id must never reach the roster row"
        );
    }

    /// `folder_member_actors` is the single UniFFI-visible derivation site for
    /// the owner-side "Shared with" filter — every native app's roster render
    /// AND its `folder-shared-badge` count (`.len()`) come from this, replacing
    /// 3 independent local `role == "member"` filters.
    #[test]
    fn folder_member_actors_drops_the_owner_row_keeping_order() {
        let mk = |actor_id: &str, role: &str| FfiFolderActorMember {
            actor_id: actor_id.into(),
            handle: String::new(),
            role: role.into(),
            display: String::new(),
            access: None,
            byte_cap: None,
            bytes_used: None,
            remote: false,
        };
        let roster = vec![mk("aa", "owner"), mk("bb", "member"), mk("cc", "member")];
        let members = folder_member_actors(roster);
        assert_eq!(
            members
                .iter()
                .map(|m| m.actor_id.as_str())
                .collect::<Vec<_>>(),
            vec!["bb", "cc"]
        );
    }

    #[test]
    fn folder_member_actors_on_an_owner_only_roster_is_empty() {
        let owner_only = vec![FfiFolderActorMember {
            actor_id: "aa".into(),
            handle: String::new(),
            role: "owner".into(),
            display: String::new(),
            access: None,
            byte_cap: None,
            bytes_used: None,
            remote: false,
        }];
        assert!(folder_member_actors(owner_only).is_empty());
    }

    #[test]
    fn device_and_member_mirrors_carry_hex_device_id() {
        let d = FfiFolderDevice {
            device_id: "ab".repeat(32),
            label: "Laptop".into(),
            last_change_at: 1_700_000_000,
            change_count: 12,
        };
        let m = FfiFolderMember {
            device_id: "cd".repeat(32),
            label: "Phone".into(),
            originates: true,
            accepts: false,
            applies_deletes: false,
        };
        assert_eq!(d.device_id.len(), 64);
        assert_eq!(d.change_count, 12);
        assert_eq!(m.device_id.len(), 64);
    }

    /// A ref the row helper renders is accepted; a bare set name (a legacy
    /// apple domain identifier) and a truncated foreign ref are not.
    #[cfg(feature = "folders-author")]
    #[test]
    fn folder_ref_is_valid_accepts_rendered_refs_and_refuses_names() {
        let local = folder_ref_for_row(7, None, None).expect("own row resolves");
        assert!(folder_ref_is_valid(local));
        let foreign = folder_ref_for_row(7, Some("ab".repeat(16)), Some("https://h".into()))
            .expect("foreign row resolves");
        assert!(folder_ref_is_valid(foreign));
        assert!(!folder_ref_is_valid("docs".into()));
        assert!(!folder_ref_is_valid("foreign:abcd".into()));
    }

    /// The scoped render/parse pair round-trips through the FFI strings, two
    /// accounts' `local:1` come out distinct, and a bare ref, the pre-encoding
    /// spelling or a bad actor yields nothing on either side.
    #[cfg(feature = "folders-author")]
    #[test]
    fn actor_scoped_folder_ref_ffi_round_trips_and_refuses_unscoped() {
        let a = "aa".repeat(32);
        let b = "BB".repeat(32);
        let local = folder_ref_for_row(1, None, None).expect("own row resolves");
        let scoped_a = actor_scoped_folder_ref(a.clone(), local.clone()).expect("renders");
        assert_eq!(
            scoped_a,
            FfiActorScopedFolderRef {
                scoped_id: format!("local%3A1@{a}"),
                actor_id_hex: a.clone(),
                folder_id: local.clone(),
                folder_component: "local%3A1".into(),
            }
        );
        let scoped_b = actor_scoped_folder_ref(b.clone(), local.clone()).expect("renders");
        assert_ne!(scoped_a.scoped_id, scoped_b.scoped_id);
        assert_eq!(
            actor_scoped_folder_ref_parse(scoped_a.scoped_id.clone()),
            Some(scoped_a.clone()),
            "parse hands back the same record the render did"
        );
        assert_eq!(
            actor_scoped_folder_ref_parse(scoped_b.scoped_id).map(|s| s.actor_id_hex),
            Some(b.to_lowercase()),
            "the actor half is rendered lowercase whatever case came in"
        );
        assert_eq!(
            actor_scoped_folder_ref_parse(local.clone()),
            None,
            "bare ref"
        );
        assert_eq!(
            actor_scoped_folder_ref_parse(format!("local:1@{a}")),
            None,
            "the pre-2026-09-29 spelling"
        );
        assert_eq!(actor_scoped_folder_ref_parse("docs".into()), None, "name");
        assert_eq!(
            actor_scoped_folder_ref("abcd".into(), local),
            None,
            "short actor"
        );
        assert_eq!(
            actor_scoped_folder_ref(a, "docs".into()),
            None,
            "non-ref set"
        );
    }
}
