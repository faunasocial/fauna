//! **Engine content-key binding** — the decision the owner's sync host makes,
//! from a `fauna.folders.list` [`FolderSummary`] + the owner's folder-keys
//! custody, about what key material a live [`SyncEngine`] must be constructed
//! with for a given folder (Slice-3 piece **5d(c)**).
//!
//! Loading the keys requires (1) the nest to tell the host a set is bound — the
//! [`FolderSummary::mls_group_id`] projection — and (2) the host to look up the
//! M2 content-key history from custody. This module is that second half: the
//! pure, deterministic resolver every engine-building host runs — the
//! identity-holding apps, the File Provider host and the desktop sync agent
//! (`crate::engine_keys`) — so the security-critical **fail-closed** decision
//! lives in one place (priority #2), not re-derived per host.
//!
//! **fauna-mls-free**: the summary carries the *raw* MLS group id, but custody is
//! keyed by the derived channel id — [`channel_id_for_group`], the one derivation
//! `fauna-mls`'s `ChannelId::from_group_id` also runs — so a bearer-only host
//! resolves its keys without linking MLS.
//!
//! Authority: `docs/goal/architecture/mls-group-key-material.md` § M2 content-key
//! mechanism → *Generation-stamping + selection on read* (the engine selects
//! `keys_for(version)` — every same-version candidate — and **fails closed** if the holder lacks that generation).

use fauna_core::data::FoldersConfig;
use fauna_core::folder_keys::{
    FolderContentKeys, FolderEngineKeys, FolderRef, channel_id_for_group, serve_custody_channel_id,
};
use fauna_core::identity::ActorId;
use fauna_protocol::folders::{AttestationMemory, FolderSummary};

/// What content-key material a [`SyncEngine`] should be constructed with for one
/// folder — resolved by [`resolve_engine_key_binding`].
///
/// The three arms exist to make the **fail-closed** posture impossible to get
/// wrong at the call site. A *bound* set whose keys are not (yet) in custody
/// MUST still construct the engine bound (`mls_group_id = Some`,
/// `content_keys = None`) so `content_seal_root` / `content_open_root` refuse to
/// seal or read shared content in plaintext (FS-BIND-5) — it must **never** be
/// treated as [`Unbound`](Self::Unbound), which would seal under the owner-only
/// `epoch_secret` path. [`engine_args`](Self::engine_args) encodes exactly that
/// mapping so a consumer cannot accidentally downgrade a bound set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineKeyBinding {
    /// Owner-only set — no cross-user group binding. Engine args:
    /// `(mls_group_id = None, content_keys = None)`.
    Unbound,
    /// Bound cross-user shared set whose content-key history is held in custody.
    /// Engine args: `(Some(mls_group_id), Some(content_keys))`.
    Bound {
        /// The raw MLS group id (the engine's bound-marker), decoded from the
        /// summary's hex `mls_group_id`.
        mls_group_id: Vec<u8>,
        /// The full generation history to seal current uploads under and open
        /// any prior-generation chunk with.
        content_keys: FolderContentKeys,
    },
    /// Bound set whose content keys are **not** in custody — a removed member, an
    /// as-yet-unsynced generation, or a startup race. Engine args:
    /// `(Some(mls_group_id), None)` so the engine **fails closed** (never seals
    /// shared content in plaintext), never `(None, None)`.
    BoundKeysMissing {
        /// The raw MLS group id (still the bound-marker, so the engine fails
        /// closed rather than degrading to the owner-only path).
        mls_group_id: Vec<u8>,
    },
    /// **WebDAV-served, unshared** set (custody calls it served —
    /// [`custody_served`] — and it has no MLS group) whose content keys are
    /// held in custody at the serve pseudo-channel
    /// ([`serve_custody_channel_id`] — `webdav-server.md` § Key model custody
    /// note). Engine args: `(None, Some(content_keys))` — the engine's
    /// `content_seal_root`/`content_open_root` prefer present content keys
    /// regardless of the bound-marker, so uploads seal under `current` and reads
    /// try every `keys_for(version)` candidate exactly as for a shared set.
    ServedUnshared {
        /// The full generation history (blob-provisioned to the MDA, re-seal
        /// target for the back-catalogue).
        content_keys: FolderContentKeys,
    },
    /// Served by custody's word, unshared, but custody holds **no** keys for
    /// the serve pseudo-channel — a stamped entry whose keys have not joined
    /// yet (serve-on writes both in one custody write, so this is a sync lag,
    /// never a state). There is **no engine representation** for
    /// "served-but-keyless" (the fail-closed bound-marker is the MLS group id,
    /// which this set doesn't have), so the consumer must **not construct an
    /// engine at all** ([`Self::engine_args`] returns `None`): running it
    /// owner-path would seal new uploads under `BackupKey`/plaintext — the
    /// FS-BIND-5 degradation a served set must never take.
    ServedKeysMissing,
}

impl EngineKeyBinding {
    /// The `(mls_group_id, content_keys)` pair to pass straight into
    /// `SyncEngine::new`, or **`None` when no engine may run at all**
    /// ([`ServedKeysMissing`](Self::ServedKeysMissing) — fail closed by not
    /// syncing). This is the single mapping from a binding to engine arguments —
    /// it guarantees a [`BoundKeysMissing`](Self::BoundKeysMissing) set yields
    /// `Some((Some(raw), None))` (bound + keyless = in-engine fail-closed) and a
    /// served-keyless set yields `None`, so a caller cannot construct a served or
    /// bound set that seals its content on the owner path.
    pub fn engine_args(self) -> Option<(Option<Vec<u8>>, Option<FolderContentKeys>)> {
        match self {
            EngineKeyBinding::Unbound => Some((None, None)),
            EngineKeyBinding::Bound {
                mls_group_id,
                content_keys,
            } => Some((Some(mls_group_id), Some(content_keys))),
            EngineKeyBinding::BoundKeysMissing { mls_group_id } => Some((Some(mls_group_id), None)),
            EngineKeyBinding::ServedUnshared { content_keys } => Some((None, Some(content_keys))),
            EngineKeyBinding::ServedKeysMissing => None,
        }
    }

    /// Whether this set is bound to a cross-user shared group (either arm with a
    /// group id) — i.e. whether the engine will treat it as a shared set.
    pub fn is_bound(&self) -> bool {
        matches!(
            self,
            EngineKeyBinding::Bound { .. } | EngineKeyBinding::BoundKeysMissing { .. }
        )
    }
}

/// The custody key for `summary`, when the set is content-keyed NOW: the
/// derived `ChannelId` for a shared set, the serve pseudo-channel for an
/// owned set **custody calls served** with no group, `None` for a plain
/// owner-only set. The one resolution every consumer shares — the engine-key
/// resolver below and the `WebdavKeysBlob` reconciler — so a set's custody
/// identity can never be derived two different ways.
///
/// **The served arm is the owner's word, never the nest's**
/// (`writer-signed-change-records.md` ruling (7)(b)(ii) rule (2)): it answers
/// from `cfg` ([`crate::custody::channel_served`] at the pseudo-channel), and
/// `summary.webdav_enabled` selects nothing. Keys resting at the
/// pseudo-channel never meant served (`paywall_set` keys an unshared set
/// there too); the serve stamps say whether that identity's DAV plane is
/// open.
///
/// `Err` when `summary.mls_group_id` is `Some` but not valid hex (a malformed
/// nest projection) — treat as indeterminate/fail-closed, never as `None`.
pub fn custody_channel_for(
    summary: &FolderSummary,
    cfg: &FoldersConfig,
) -> Result<Option<[u8; 32]>, hex::FromHexError> {
    if summary.mls_group_id.is_some() {
        return owned_custody_channel(summary).map(Some);
    }
    let pseudo = serve_custody_channel_id(&summary.name);
    Ok(
        (summary.role.as_deref() != Some("member") && crate::custody::channel_served(cfg, &pseudo))
            .then_some(pseudo),
    )
}

/// The channel a row's content keys rest at **whether or not the set is
/// served**: the derived `ChannelId` of its group, else the serve
/// pseudo-channel of its name — where a served, once-served or paywalled
/// unshared set's entry is keyed. [`custody_channel_for`] is this narrowed to
/// the sets that are content-keyed now.
pub fn owned_custody_channel(summary: &FolderSummary) -> Result<[u8; 32], hex::FromHexError> {
    match summary.mls_group_id.as_deref() {
        Some(hex_gid) => Ok(channel_id_for_group(&hex::decode(hex_gid)?)),
        None => Ok(serve_custody_channel_id(&summary.name)),
    }
}

/// Whether custody calls `summary`'s set WebDAV-served — the reader's
/// exemption, the snapshot's toggle and the provisioner's filter (ruling
/// (7)(b)(ii) rule (2)): the owner's entry at the set's custody channel, a
/// member's received copy at the real channel (a member row with no group
/// holds none). The nest's `webdav_enabled` is never consulted. A malformed
/// group id reads not served.
#[must_use]
pub fn custody_served(summary: &FolderSummary, cfg: &FoldersConfig) -> bool {
    if summary.mls_group_id.is_none() && summary.role.as_deref() == Some("member") {
        return false;
    }
    owned_custody_channel(summary)
        .is_ok_and(|channel| crate::custody::channel_served(cfg, &channel))
}

/// Whether custody serves ≥1 of the owner's `rows` (owner-scoped
/// `fauna.folders.list` rows) — the MUA URL row's signal (ruling (7)(b)(ii)
/// rule (2)). The keys-blob reconcile's fold: rows named from custody by
/// hash (a sealed row rests no name), then [`custody_served`] on each owned
/// row; a member row is never the reader's own served set.
#[must_use]
pub fn custody_serves_any(rows: Vec<FolderSummary>, cfg: &FoldersConfig) -> bool {
    crate::custody::named_from_custody(rows, cfg)
        .iter()
        .any(|s| s.role.as_deref() != Some("member") && custody_served(s, cfg))
}

/// The custody answer to the mail-settings machine's `WebdavServedSets`
/// seam: [`custody_serves_any`] over the account's folder-key custody, read
/// fresh per ask. An unreadable custody answers `false`.
#[cfg(feature = "mail-settings")]
pub struct CustodyServedSets(pub std::sync::Arc<dyn crate::key_reader::FolderKeyStore>);

#[cfg(feature = "mail-settings")]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_client_mail_settings::WebdavServedSets for CustodyServedSets {
    async fn serves_any(&self, rows: Vec<FolderSummary>) -> bool {
        match crate::key_reader::FolderKeyReader::load(&*self.0).await {
            Ok(cfg) => custody_serves_any(rows, &cfg),
            Err(_) => false,
        }
    }
}

/// Where a host keeps the **MLS-recorded folder owner** of a channel — the
/// `folder_channel_owner` marker `fauna-mls` stamps at folder-group mint and
/// folder-Welcome join and re-points on the owner's verified succession
/// (`federation.md` § Cross-nest shared folders + channel append). Abstract
/// here because this crate's base build is fauna-mls-free: an MLS-holding host
/// wraps its engine, a bearer-only host has none.
pub trait FolderChannelOwners: Sync {
    /// The recorded owner of the channel with this id, `None` when unstamped.
    fn folder_channel_owner(&self, channel_id: &[u8; 32]) -> Option<ActorId>;
}

/// The identity a seat verifies the owner-attested declassification against —
/// `FolderSummary::judge_declassification`'s `trusted_owner`, resolved from
/// sources **the nest cannot forge** (`encryption-at-rest.md` § Readable
/// classes → *The declassification is owner-ATTESTED*). One resolver for every
/// reader, so the live seat resolver, the engine binding decision, the pushed
/// agent capability and Media's seam cannot disagree about whose signature
/// arms a folder.
pub struct DeclassificationAnchor<'a> {
    /// This seat's own actor id — the trusted owner of every folder its
    /// account owns (an app derives it from the identity seed; the bearer-only agent holds it in its pushed capability).
    pub own: ActorId,
    /// The host's MLS state, for a **member** row: the trusted owner is the
    /// folder channel's recorded owner — never the `owner_actor_id` /
    /// `owner_handle` the nest fills on the row. `None` on a host that holds
    /// no MLS state (the sync agent's bearer-only engines, the File Provider
    /// helper): its member seats have no anchor and **seal**.
    pub channel_owners: Option<&'a dyn FolderChannelOwners>,
}

impl<'a> DeclassificationAnchor<'a> {
    /// A host with no MLS state: own folders verify, member seats seal.
    #[must_use]
    pub fn own_only(own: ActorId) -> Self {
        Self {
            own,
            channel_owners: None,
        }
    }

    /// The trusted owner for `summary`: the seat's own actor id for a row its
    /// account owns (any `role` but `member` — an owner row, or a row with no
    /// role, which reads as owner), the channel's MLS-recorded owner for a member row.
    /// `None` seals: a member row on an MLS-less host, a member row whose
    /// channel this host never stamped, or a
    /// member row whose group id does not resolve to a channel at all.
    #[must_use]
    pub fn trusted_owner_for(&self, summary: &FolderSummary) -> Option<ActorId> {
        if summary.role.as_deref() != Some("member") {
            return Some(self.own);
        }
        summary.mls_group_id.as_ref()?;
        let channel = owned_custody_channel(summary).ok()?;
        self.channel_owners?.folder_channel_owner(&channel)
    }
}

/// Retired M2 content-key generations for `summary` when it is **currently**
/// owner-only (group-less, and not served by custody's word —
/// [`custody_channel_for`]) but the owner's custody still holds keys
/// at its serve pseudo-channel — evidence the set WAS served and later
/// unflagged (`webdav-server.md` § Key model, Revocation: unflagging a set's
/// serve toggle rotates its content key rather than forgetting it — the
/// existing rotate machinery, exactly a member removal).
///
/// Read-only candidates for [`fauna_sync_engine::SyncEngine::set_retired_content_keys`]
/// (this crate does not depend on `fauna-sync-engine`, so the setter is the
/// caller's) — never the set's *live* [`EngineKeyBinding`], which
/// [`resolve_engine_key_binding`] already, correctly, resolves to
/// [`EngineKeyBinding::Unbound`] for exactly this shape (the retired
/// generations must never re-trip the content-keyed gate that made the set
/// eligible for the owner path in the first place).
///
/// `None` for every other shape — bound, still served, or genuinely never
/// served (no custody at the pseudo-channel at all) — so a set with no serve
/// history costs nothing beyond the two summary-field checks.
pub fn retired_serve_custody(
    cfg: &FoldersConfig,
    summary: &FolderSummary,
) -> Option<FolderContentKeys> {
    if !matches!(custody_channel_for(summary, cfg), Ok(None)) {
        return None;
    }
    crate::custody::content_keys(cfg, &serve_custody_channel_id(&summary.name))
}

/// The [`FolderRef`] a client should stamp on a folder binding for a folder
/// row, from the three fields every app's summary type carries.
///
/// Takes the fields rather than a summary struct because the seven apps render
/// from two different summary types (`fauna_protocol::folders::FolderSummary`
/// and the `fauna-devices-machine` snapshot twin) — and because the rule is one
/// rule, which must not be re-derived per app (priority #2; a per-app copy is how
/// linux and tui would drift on which arm a member row takes).
///
/// The arm is decided by **where the set lives**, not by the caller's role in it:
///
/// * `home_nest_url` present ⇒ the set is homed on *another* nest, so this
///   holder has no `folders` row for it and its identity is the derived
///   `ChannelId` ([`FolderRef::Foreign`]).
/// * otherwise ⇒ the row came from this nest's own projection, owner-scoped or
///   member-visible, and `id` is that nest's `folders` PK
///   ([`FolderRef::Local`]). A same-nest *member* row takes this arm too: it has
///   a perfectly good local row id, and the PK is stable where the channel would
///   change if the set were ever re-bound to a new group.
///
/// `None` when the row claims a foreign home but its group id is missing or
/// malformed — a half-described row yields no identity rather than a wrong one.
/// Every caller **refuses the bind** on `None` (fail closed): a folder binding
/// is keyed by its ref alone, and the name-keyed fallback that used to absorb
/// this case was retired 2026-09-24 (the compat-remnant sweep) — the name is a
/// label two sets can share.
#[must_use]
pub fn folder_ref_for_row(
    id: i64,
    mls_group_id_hex: Option<&str>,
    home_nest_url: Option<&str>,
) -> Option<FolderRef> {
    if home_nest_url.is_none() {
        return Some(FolderRef::Local(id));
    }
    let raw = hex::decode(mls_group_id_hex?).ok()?;
    Some(FolderRef::Foreign(channel_id_for_group(&raw)))
}

/// Resolve the [`EngineKeyBinding`] for `summary` from the owner's custody
/// `cfg` (the `fauna.state.folder-keys` fold).
///
/// Pure + deterministic (no network, no RNG): the caller loads the custody
/// once (through a [`crate::FolderKeyReader`]) and calls this per summary from
/// `fauna.folders.list`, then feeds [`EngineKeyBinding::engine_args`] into
/// `SyncEngine::new`.
///
/// `Err` only when `summary.mls_group_id` is `Some` but not valid hex (a
/// malformed nest projection). The caller MUST treat that as fail-closed —
/// bound-but-unusable — not as [`Unbound`](EngineKeyBinding::Unbound); the raw
/// error is surfaced so the host can log the corruption.
pub fn resolve_engine_key_binding(
    cfg: &FoldersConfig,
    summary: &FolderSummary,
) -> Result<EngineKeyBinding, hex::FromHexError> {
    let Some(hex_gid) = summary.mls_group_id.as_deref() else {
        // No MLS group. A set custody calls WebDAV-served is still
        // content-keyed — custody at the serve pseudo-channel
        // (`webdav-server.md` § Key model custody note); keys absent there is
        // served-but-keyless → no engine (fail closed), never the owner path.
        // The nest's flag selects nothing (ruling (7)(b)(ii) rule (2)).
        if matches!(custody_channel_for(summary, cfg), Ok(Some(_))) {
            return Ok(
                match crate::custody::content_keys(cfg, &serve_custody_channel_id(&summary.name)) {
                    Some(content_keys) => EngineKeyBinding::ServedUnshared { content_keys },
                    None => EngineKeyBinding::ServedKeysMissing,
                },
            );
        }
        return Ok(EngineKeyBinding::Unbound);
    };
    let raw = hex::decode(hex_gid)?;
    let channel_id = channel_id_for_group(&raw);
    Ok(match crate::custody::content_keys(cfg, &channel_id) {
        Some(content_keys) => EngineKeyBinding::Bound {
            mls_group_id: raw,
            content_keys,
        },
        None => EngineKeyBinding::BoundKeysMissing { mls_group_id: raw },
    })
}

/// The nest's unrendered rows ([`crate::FoldersClient::list_owned_and_shared_wire`])
/// with each sealed row's NAME filled in, for an **engine host** — the sync
/// agent's content-key resolution and the shared engine build, which take a
/// set's name from the row they bind. Since schema 114 a sealed set's row rests
/// no plaintext name (`path-sealing.md` § the set-name plane): a wire row's
/// blank name built an engine that addressed the nest by the hash of the empty
/// string, and the rendered list on a client with no label custody omitted the
/// row, so no engine was built at all.
///
/// Pure, over the custody the host already read — no label custody, no second
/// round trip, and nothing from this crate's `mls` graph, which the bearer-only
/// agent does not build:
/// - **the holder's own row** is named from custody by hash
///   ([`crate::custody::named_from_custody`] — custody holds the name of every
///   set this holder created);
/// - **a member's row** opens `name_sealed` under the set's content keys, the
///   very binding [`resolve_engine_key_binding`] hands the engine for the set's
///   bytes — so the audience that opens a set's files is the one that names it.
///
/// A sealed row neither names is dropped: no engine for it until an edge
/// re-reads (custody not walked yet, or the share's name stamp not landed).
/// A row that still carries its plaintext passes through.
pub fn named_for_engine_host(rows: Vec<FolderSummary>, cfg: &FoldersConfig) -> Vec<FolderSummary> {
    use fauna_core::path_crypto::SealedLabelRender;
    rows.into_iter()
        .filter_map(|mut row| {
            if !row.name.is_empty() {
                return Some(row);
            }
            if row.role.as_deref() != Some("member") {
                return crate::custody::named_from_custody(vec![row], cfg).pop();
            }
            let Ok(EngineKeyBinding::Bound {
                mls_group_id,
                content_keys,
            }) = resolve_engine_key_binding(cfg, &row)
            else {
                return None;
            };
            let keys = fauna_core::file_download::FileDownloadKeys {
                mls_group_id: Some(mls_group_id),
                content_keys: Some(content_keys),
                ..Default::default()
            };
            match fauna_core::label_custody::render_set_name(
                &keys,
                row.name_sealed.as_deref().map(|b| &b[..]),
                "",
                row.name_hash.as_deref().map(|b| &b[..]),
            ) {
                SealedLabelRender::Sealed(name) | SealedLabelRender::Plaintext(name)
                    if !name.is_empty() =>
                {
                    row.name = name;
                    Some(row)
                }
                _ => None,
            }
        })
        .collect()
}

/// Resolve the [`FolderEngineKeys`] for **every** `summary` from the holder's
/// already-unsealed `cfg`, tagging each with its set identity — the same-nest half
/// of [`crate::engine_keys::engine_keys_from`], which every engine-building host
/// runs over `fauna.folders.list`'s summaries (the desktop sync agent at each of
/// its re-resolve edges).
///
/// Each entry preserves the per-set fail-closed mapping of
/// [`EngineKeyBinding::engine_args`]: a bound-but-keyless set yields
/// `Some(mls_group_id)` + `None` keys, never `(None, None)`.
///
/// `Err` only on the **first** summary whose `mls_group_id` is `Some` but
/// malformed hex (a corrupt nest projection) — a batch that cannot even be read
/// is never silently shipped; the caller keeps its previous capability and
/// retries.
///
/// A **served-but-keyless** set ([`EngineKeyBinding::ServedKeysMissing`] —
/// `FolderEngineKeys` has no fail-closed representation for it: shipping
/// `(None, None)` would make the bearer service seal it on the owner path) is
/// **omitted from the batch (logged at `WARN`) rather than withholding the
/// whole fleet** — the batch ships every set it *can* resolve. This is safe
/// only because absent-from-the-blob already means no engine, never owner-only
/// (`sync-agent.md` § Control plane split, the pending-vs-resolved paragraph):
/// `ResolvedContentKeys::keys_for` on the agent side
/// (`bins/fauna-sync-agent/src/content_keys.rs`) withholds exactly this one set until a
/// later push resolves its keys, the same outcome a whole-batch `Err` used to
/// force on every other set too. Before that rule existed (pre-2026-09-08),
/// "absent" meant owner-only, so the all-or-nothing `Err` was the only
/// fail-closed choice — narrowing the blast radius here would have been the
/// silent-wrong-key bug, not a fix.
///
/// `adoption_markers` are this device's (`FolderKeyReader::adoption_markers`):
/// an owned set whose lineage holds one carries it to the engine
/// (`FolderEngineKeys::adoption_marker`, `writer-signed-change-records.md`
/// ruling (11)(d)).
pub fn resolve_engine_key_bindings(
    cfg: &FoldersConfig,
    own: ActorId,
    summaries: &[FolderSummary],
    adoption_markers: &[[u8; 32]],
) -> Result<Vec<FolderEngineKeys>, EngineKeyBindingError> {
    summaries
        .iter()
        .filter_map(|summary| {
            let binding = match resolve_engine_key_binding(cfg, summary) {
                Ok(binding) => binding,
                Err(e) => return Some(Err(EngineKeyBindingError::MalformedGroupId(e))),
            };
            let Some((mls_group_id, content_keys)) = binding.engine_args() else {
                // ServedKeysMissing: no fail-closed engine representation exists
                // without a group id. Omit just this set rather than the whole
                // batch — `ResolvedContentKeys::keys_for` already withholds an unnamed set's
                // engine (ratified 2026-09-08), so this reaches the identical
                // fail-closed outcome at one-set blast radius instead of the
                // whole fleet's.
                tracing::warn!(
                    folder = %summary.name,
                    "served set holds no content keys at its serve custody channel yet; \
                     omitting it from this push rather than withholding the whole batch — \
                     it gets no engine until a later push resolves its keys"
                );
                return None;
            };
            // The set's binding pair (ruling (g)), on every arm. The group id
            // already decoded above, so the channel resolves; the name keys
            // only the caller's OWN sets.
            let owner_name =
                (summary.role.as_deref() != Some("member")).then_some(summary.name.as_str());
            let channel = custody_channel_for(summary, cfg).ok().flatten();
            let lineage = crate::custody::set_lineage(cfg, owner_name, channel.as_ref());
            let carried = carried_lineage(&lineage, owner_name.is_some(), adoption_markers);
            // The declassification anchor, from the session's own identity —
            // `own`, the identity the holder unsealed this config AS, never a
            // row field and never an id read off the config
            // (`config-dissolution.md` § Phases and gates → *Bounded rows* →
            // *The ledger*).
            // This producer holds no MLS state, so a member row has no anchor
            // here and its entry ships sealed; the engine seats judge for
            // themselves at build and every tick (`fauna-sync-engine`
            // `decide_engine_content_binding`, `config::judge_seat_declassification`).
            let trusted_owner = DeclassificationAnchor::own_only(own).trusted_owner_for(summary);
            Some(Ok(FolderEngineKeys {
                folder: summary.name.clone(),
                // The unambiguous key (residual R1 (account-data-plane.md § The ratified decisions)): a summary comes from a nest
                // projection, so it always has a row on *this* nest — owner-scoped
                // or member-visible, both carrying `folders.id` verbatim from the
                // one table. That is `FolderRef::Local` by construction.
                folder_id: FolderRef::Local(summary.id).to_wire(),
                mls_group_id,
                content_keys,
                // Phase 4: the declassification flag this entry carries is the
                // **verifier's verdict** over the row — a genuine attestation by
                // the trusted owner over this row's id and name — never the
                // nest's bare claim. Judged under a FRESH
                // memory: this producer holds no seat's replay floor, so no
                // engine arms off this flag alone — every engine seat judges
                // under its own persisted floor (`build_engine`'s resolver and
                // the resident tick).
                public_audience: summary
                    .judge_declassification(
                        &summary.name,
                        trusted_owner.as_ref(),
                        AttestationMemory::default(),
                    )
                    .0,
                // The served-toggle read candidate: `None` for every
                // set with no serve history, `Some` only for a group-less,
                // now-unserved set whose custody still holds a served window's
                // rotated-out generation — exactly `retired_serve_custody`'s
                // contract.
                retired_content_keys: retired_serve_custody(cfg, summary),
                // Same-nest by construction: these come from a nest projection,
                // which can only ever describe sets that nest claims.
                ..carried
            }))
        })
        .collect()
}

/// Resolve the [`FolderEngineKeys`] for every **cross-nest** set this holder is
/// a member of — the foreign half of the pushed blob.
///
/// A foreign set exists **only** in the member's own folder-keys custody
/// ([`FoldersConfig::foreign_sets`], written at share-accept): the
/// member's own nest holds no row for it, so no `fauna.folders.list`
/// projection — owner-scoped or member-visible — can ever contain it. That is
/// why this is a second source unioned into the batch rather than more summaries
/// fed to [`resolve_engine_key_bindings`], and it is the whole reason a bound
/// foreign folder ran **unbound** before this existed.
///
/// Each entry carries the same fail-closed `(mls_group_id, content_keys)` posture
/// as a same-nest shared set — a foreign set is always bound (it *is* an MLS
/// group), so a holder without custody keys yet yields `Some(gid)` + `None`,
/// never `(None, None)` — plus the `(home_nest_url, channel_id_hex)` routing pair
/// that makes it foreign.
///
/// **A name-less record is skipped.** `ForeignFolder.set_name` is `None` when
/// the seal was unopenable, and the agent seam addresses sets by
/// name — an entry keyed on `""` would collide with every other name-less record
/// and could hand one foreign set's keys to another. Skipping means such a set is
/// simply not bindable (its *reads* still work, keyed by channel), which is the
/// fail-safe direction.
///
/// **Access is deliberately NOT consulted.** `ForeignFolder.access` is
/// advisory-for-UI only (D2 — `federation.md` § Cross-nest → *Recipient-side
/// access discovery*), so filtering the blob on it would make key material depend
/// on a value the design forbids treating as authoritative: a stale `reader`
/// would silently unbind a legitimately-bound writer's engine, which is the
/// silent-wrong-key class this track has already paid for twice. Whether a write
/// is allowed is settled by the home nest at the eager `write_token.get` and at
/// `require_foreign_writer` — loudly, and at the moment it matters.
pub fn resolve_foreign_engine_key_bindings(cfg: &FoldersConfig) -> Vec<FolderEngineKeys> {
    crate::custody::live_foreign_sets(cfg)
        .filter_map(|foreign| {
            let folder = foreign.set_name.clone()?;
            Some(FolderEngineKeys {
                folder,
                // The unambiguous key (residual R1). A foreign set has **no** row
                // on this holder's nest, so it has no `Local` id to take; its
                // `channel_id` is the identity custody and the federated read kinds
                // already key it by, and it is stable for the set's shared life.
                folder_id: FolderRef::Foreign(foreign.channel_id).to_wire(),
                mls_group_id: Some(foreign.mls_group_id.clone()),
                content_keys: crate::custody::content_keys(cfg, &foreign.channel_id),
                home_nest_url: Some(foreign.home_nest_url.clone()),
                channel_id_hex: Some(hex::encode(foreign.channel_id)),
                // The home nest's identity root for the byte-plane SPKI pin and the
                // owner-chosen cadence ride the same blob — both sourced from the
                // member's foreign-set row (seeded on the Welcome, refreshed on
                // every federated read reply), both `None` for a relay-unaware home.
                home_nest_actor_id: foreign.home_nest_actor_id.clone(),
                // A foreign set's audience is the HOME nest's fact, and the
                // member's foreign-set row does not carry it — so a foreign
                // engine always seals (the fail-safe direction). A cross-nest
                // public-folder write arm is a later federation leg; when it
                // comes, the anchor is the channel's MLS-recorded owner, which
                // this MLS-less producer cannot supply either.
                public_audience: false,
                // A foreign set is always bound (it *is* an MLS group) — never
                // the group-less, now-unserved shape `retired_serve_custody`
                // exists for — so there is no retired serve-toggle generation
                // to carry here.
                retired_content_keys: None,
                // A member's received copy of the owner's nonce and lineage
                // (the owner's envelope, ruling (11)(b)).
                ..carried_lineage(
                    &crate::custody::set_lineage(cfg, None, Some(&foreign.channel_id)),
                    false,
                    &[],
                )
            })
        })
        .collect()
}

/// The nonce fields of one set's [`FolderEngineKeys`] (every other field left
/// default): the live nonce with its minter and the lineage with theirs
/// (`writer-signed-change-records.md` ruling (11)(b)); on an OWNED set, the
/// lineage's nonces as the re-record leg's input (`retired_set_nonces`, empty
/// for a member) and this device's adoption marker for the set, when one of
/// `adoption_markers` names a nonce of its lineage (ruling (11)(d)).
fn carried_lineage(
    lineage: &fauna_core::folder_keys::SetNonceLineage,
    owned: bool,
    adoption_markers: &[[u8; 32]],
) -> FolderEngineKeys {
    let retired: Vec<[u8; 32]> = lineage.retired.iter().map(|r| r.nonce).collect();
    FolderEngineKeys {
        set_nonce: lineage.live,
        set_nonce_minted_by: lineage.live_minted_by,
        retired_lineage: lineage.retired.clone(),
        // The serve window beside the nonce, on every arm (ruling (7)(b)(ii)
        // rule (4)): the agent's custody re-read rebuilds the binding edge
        // when the stamps move and the roster row does not.
        served_at: lineage.served_at,
        unserved_at: lineage.unserved_at,
        adoption_marker: owned
            .then(|| {
                adoption_markers
                    .iter()
                    .find(|m| retired.contains(m))
                    .copied()
            })
            .flatten(),
        retired_set_nonces: if owned { retired } else { Vec::new() },
        ..Default::default()
    }
}

/// A failure resolving a batch of engine-key bindings
/// ([`resolve_engine_key_bindings`]). A served-but-keyless summary is **not**
/// this — it is omitted from the batch (logged) rather than erring; the only
/// way this batch fails is a corrupt nest projection.
#[derive(Debug)]
pub enum EngineKeyBindingError {
    /// A summary's `mls_group_id` projection is not valid hex (nest-side
    /// corruption) — indeterminate, fail closed.
    MalformedGroupId(hex::FromHexError),
}

impl core::fmt::Display for EngineKeyBindingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MalformedGroupId(e) => write!(f, "malformed mls_group_id projection: {e}"),
        }
    }
}

impl std::error::Error for EngineKeyBindingError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custody::record_new_set;
    use fauna_core::identity::ActorId;

    /// The holder's own identity — the declassification anchor.
    const OWN: ActorId = ActorId([7u8; 32]);

    fn cfg() -> FoldersConfig {
        FoldersConfig::default()
    }

    /// The arm is decided by **where the set lives**, not by the caller's role.
    /// A same-nest *member* row has a perfectly good local row id and must take
    /// the `Local` arm: only a cross-nest set genuinely has no row here.
    #[test]
    fn row_ref_keys_on_where_the_set_lives_not_on_the_callers_role() {
        let gid = hex::encode(b"raw-group-id");
        let channel = channel_id_for_group(b"raw-group-id");

        // Owner-only, owner-shared and same-nest member rows: all local.
        assert_eq!(
            folder_ref_for_row(42, None, None),
            Some(FolderRef::Local(42))
        );
        assert_eq!(
            folder_ref_for_row(42, Some(&gid), None),
            Some(FolderRef::Local(42)),
            "a same-nest shared set still has a local row id"
        );

        // Cross-nest: no local row exists, so the derived channel is the identity.
        assert_eq!(
            folder_ref_for_row(0, Some(&gid), Some("https://home.example")),
            Some(FolderRef::Foreign(channel))
        );
    }

    /// A row claiming a foreign home but carrying no usable group id is
    /// half-described. It must yield **no** identity — which every caller
    /// answers by refusing the bind — never a wrong one.
    #[test]
    fn a_half_described_foreign_row_yields_no_ref() {
        assert_eq!(
            folder_ref_for_row(1, None, Some("https://home.example")),
            None
        );
        assert_eq!(
            folder_ref_for_row(1, Some("not-hex"), Some("https://home.example")),
            None
        );
    }

    /// The producers must stamp the identity, or the agent has nothing to
    /// disambiguate two same-named sets with (residual R1). Same-nest entries
    /// take the row id; the foreign union takes the channel.
    #[test]
    fn both_producers_stamp_the_identity() {
        let same_nest = resolve_engine_key_bindings(&cfg(), OWN, &[summary(None)], &[]).unwrap();
        assert_eq!(same_nest[0].folder_id, FolderRef::Local(1).to_wire());

        let mut c = cfg();
        c.foreign_sets.push(fauna_core::data::ForeignFolder {
            channel_id: [0xab; 32],
            mls_group_id: b"gid".to_vec(),
            home_nest_url: "https://home.example".into(),
            home_nest_actor_id: None,
            set_name: Some("shared-docs".into()),
            access: None,
            content_key_floor: None,
            ..Default::default()
        });
        let foreign = resolve_foreign_engine_key_bindings(&c);
        assert_eq!(
            foreign[0].folder_id,
            FolderRef::Foreign([0xab; 32]).to_wire()
        );

        // And the two are distinguishable despite the shared name — the whole point.
        assert_eq!(same_nest[0].folder, foreign[0].folder);
        assert_ne!(same_nest[0].folder_id, foreign[0].folder_id);
    }

    /// A summary bound to `raw_group_id` (hex), or unbound when `None`.
    fn summary(mls_group_id: Option<Vec<u8>>) -> FolderSummary {
        FolderSummary {
            id: 1,
            name: "shared-docs".into(),
            retention_policy: None,
            cached_snapshot_count: 0,
            cached_total_bytes: 0,
            cached_last_snapshot_at: None,
            include_paths: None,
            exclude_paths: None,
            mls_group_id: mls_group_id.map(hex::encode),
            role: None,
            owner_handle: None,
            webdav_enabled: false,
            ..Default::default()
        }
    }

    #[test]
    fn unbound_summary_resolves_unbound() {
        let c = cfg();
        let binding = resolve_engine_key_binding(&c, &summary(None)).unwrap();
        assert_eq!(binding, EngineKeyBinding::Unbound);
        assert_eq!(binding.engine_args(), Some((None, None)));
    }

    #[test]
    fn bound_with_custody_resolves_bound() {
        let raw = b"raw-openmls-group-id-bytes".to_vec();
        let channel_id = channel_id_for_group(&raw);
        let mut c = cfg();
        record_new_set(&mut c, channel_id, [0x42; 32], 1_000);

        let binding = resolve_engine_key_binding(&c, &summary(Some(raw.clone()))).unwrap();
        match &binding {
            EngineKeyBinding::Bound {
                mls_group_id,
                content_keys,
            } => {
                assert_eq!(mls_group_id, &raw);
                assert_eq!(content_keys.current_key(), &[0x42; 32]);
                assert_eq!(content_keys.current_version(), 1);
            }
            other => panic!("expected Bound, got {other:?}"),
        }
        let (gid, keys) = binding.engine_args().expect("runs");
        assert_eq!(gid, Some(raw));
        assert!(keys.is_some());
    }

    #[test]
    fn bound_without_custody_fails_closed_not_unbound() {
        // Owner holds NO custody for this set's channel — a removed member, an
        // unsynced generation, or a startup race. The engine must be bound +
        // keyless (fail closed), NEVER (None, None) (which would seal plaintext).
        let raw = b"a-bound-set-with-no-keys-held".to_vec();
        let c = cfg();
        let binding = resolve_engine_key_binding(&c, &summary(Some(raw.clone()))).unwrap();
        assert_eq!(
            binding,
            EngineKeyBinding::BoundKeysMissing {
                mls_group_id: raw.clone()
            }
        );
        assert!(binding.is_bound());
        // The critical guarantee: bound-marker preserved, keys absent.
        assert_eq!(binding.engine_args(), Some((Some(raw), None)));
    }

    /// A group-less summary the NEST flags served — the owner's word is
    /// custody's ([`serve`]).
    fn served_summary(name: &str) -> FolderSummary {
        let mut s = summary(None);
        s.name = name.into();
        s.webdav_enabled = true;
        s
    }

    /// The owner's serve-on stamp on a set's keyed serve custody.
    fn serve(c: &mut FoldersConfig, name: &str) {
        assert!(crate::custody::serve_on(
            c,
            &fauna_core::folder_keys::serve_custody_channel_id(name),
            2_000
        ));
    }

    /// Ruling (7)(b)(ii) rule (2): the nest's flag selects nothing. A set the
    /// nest flags served whose custody is keyed and stamp-less (a paywall's,
    /// a set served before the build) binds owner-only, with its generations
    /// as retired read candidates.
    #[test]
    fn a_nest_flag_alone_never_makes_a_set_served() {
        let mut c = cfg();
        record_new_set(
            &mut c,
            fauna_core::folder_keys::serve_custody_channel_id("served-docs"),
            [0x42; 32],
            1_000,
        );
        let flagged = served_summary("served-docs");
        assert!(!custody_served(&flagged, &c));
        assert_eq!(custody_channel_for(&flagged, &c).unwrap(), None);
        assert_eq!(
            resolve_engine_key_binding(&c, &flagged).unwrap(),
            EngineKeyBinding::Unbound
        );
        assert!(retired_serve_custody(&c, &flagged).is_some());
        // And the owner's word serves a set the nest calls unserved.
        serve(&mut c, "served-docs");
        let mut unflagged = flagged.clone();
        unflagged.webdav_enabled = false;
        assert!(custody_served(&unflagged, &c));
        assert!(matches!(
            resolve_engine_key_binding(&c, &unflagged).unwrap(),
            EngineKeyBinding::ServedUnshared { .. }
        ));
        assert!(retired_serve_custody(&c, &unflagged).is_none());
        // A member row with no group holds no custody of its own.
        let mut member = flagged;
        member.role = Some("member".into());
        assert!(!custody_served(&member, &c));
    }

    /// Ruling (7)(b)(ii) rule (2): the MUA URL row's "serves ≥1 set" is the
    /// owner's custody's word — a nest flagging an unserved set served counts
    /// for nothing, a set custody serves counts whatever the nest's flag, and
    /// a member row is never the reader's own served set.
    #[test]
    fn serves_any_reads_custody_never_the_nest_flag() {
        let mut c = cfg();
        for name in ["served", "flagged"] {
            record_new_set(
                &mut c,
                fauna_core::folder_keys::serve_custody_channel_id(name),
                [0x42; 32],
                1_000,
            );
        }
        serve(&mut c, "served");
        let row = |name: &str, flag: bool| {
            let mut s = served_summary(name);
            s.webdav_enabled = flag;
            s
        };
        assert!(!custody_serves_any(vec![row("flagged", true)], &c));
        assert!(custody_serves_any(
            vec![row("flagged", true), row("served", false)],
            &c
        ));
        let mut member = row("served", true);
        member.role = Some("member".into());
        assert!(!custody_serves_any(vec![member], &c));
    }

    /// Ruling (7)(b)(ii) rule (4): the pushed binding carries the serve
    /// stamps on the served arm, and none for a set custody does not serve.
    #[test]
    fn resolve_bindings_carries_the_serve_stamps() {
        let mut c = cfg();
        record_new_set(
            &mut c,
            fauna_core::folder_keys::serve_custody_channel_id("served-docs"),
            [0x42; 32],
            1_000,
        );
        let row = served_summary("served-docs");
        let unserved =
            resolve_engine_key_bindings(&c, OWN, std::slice::from_ref(&row), &[]).unwrap();
        assert!(!unserved[0].webdav_served());
        serve(&mut c, "served-docs");
        let served = resolve_engine_key_bindings(&c, OWN, &[row], &[]).unwrap();
        assert!(served[0].webdav_served());
        assert_eq!(served[0].served_at, Some(2_000));
        assert_ne!(served[0], unserved[0], "a stamp move is a binding change");
    }

    #[test]
    fn served_unshared_with_custody_resolves_content_keys_without_a_group() {
        let mut c = cfg();
        record_new_set(
            &mut c,
            fauna_core::folder_keys::serve_custody_channel_id("served-docs"),
            [0x42; 32],
            1_000,
        );
        serve(&mut c, "served-docs");
        let binding = resolve_engine_key_binding(&c, &served_summary("served-docs")).unwrap();
        match &binding {
            EngineKeyBinding::ServedUnshared { content_keys } => {
                assert_eq!(content_keys.current_key(), &[0x42; 32]);
            }
            other => panic!("expected ServedUnshared, got {other:?}"),
        }
        assert!(!binding.is_bound(), "served-unshared is not group-bound");
        // Engine runs group-less but content-keyed: seals under `current`,
        // opens via `keys_for(version)` (content_seal_root prefers present keys).
        assert_eq!(binding.engine_args(), Some((None, Some(_keys(0x42)))));
    }

    #[test]
    fn served_unshared_without_custody_refuses_an_engine_entirely() {
        // Served by custody's word, no keys at the serve pseudo-channel yet
        // (the stamp joined before the generations did). There is no in-engine
        // fail-closed representation without a group id, so NO engine may run
        // — owner-path would seal new uploads under BackupKey/plaintext
        // (FS-BIND-5/FS-5DC).
        let mut c = cfg();
        c.sets.push(fauna_core::data::FolderKeyCustody {
            channel_id: Some(fauna_core::folder_keys::serve_custody_channel_id(
                "served-docs",
            )),
            served_at: Some(2_000),
            ..Default::default()
        });
        let binding = resolve_engine_key_binding(&c, &served_summary("served-docs")).unwrap();
        assert_eq!(binding, EngineKeyBinding::ServedKeysMissing);
        assert_eq!(binding.engine_args(), None, "no engine at all");
    }

    #[test]
    fn served_and_shared_resolves_via_the_real_channel() {
        // A set both shared and served works unchanged (webdav-server.md § Key
        // model custody note): the real derived ChannelId wins; the pseudo-
        // channel is only for group-less sets.
        let raw = b"raw-openmls-group-id-bytes".to_vec();
        let channel_id = channel_id_for_group(&raw);
        let mut c = cfg();
        record_new_set(&mut c, channel_id, [0x42; 32], 1_000);
        let mut s = summary(Some(raw.clone()));
        s.webdav_enabled = true;
        let binding = resolve_engine_key_binding(&c, &s).unwrap();
        assert!(matches!(binding, EngineKeyBinding::Bound { .. }));
    }

    #[test]
    fn custody_channel_for_resolves_all_three_identities() {
        let raw = b"raw-openmls-group-id-bytes".to_vec();
        let mut c = cfg();
        record_new_set(
            &mut c,
            fauna_core::folder_keys::serve_custody_channel_id("served-docs"),
            [0x42; 32],
            1_000,
        );
        serve(&mut c, "served-docs");
        // Shared → the derived ChannelId (webdav flag irrelevant).
        let mut shared = summary(Some(raw.clone()));
        shared.webdav_enabled = true;
        assert_eq!(
            custody_channel_for(&shared, &c).unwrap(),
            Some(channel_id_for_group(&raw))
        );
        // Served-unshared (custody's word) → the serve pseudo-channel.
        assert_eq!(
            custody_channel_for(&served_summary("served-docs"), &c).unwrap(),
            Some(fauna_core::folder_keys::serve_custody_channel_id(
                "served-docs"
            ))
        );
        // Plain owner-only → no custody identity.
        assert_eq!(custody_channel_for(&summary(None), &c).unwrap(), None);
        // Malformed projection → Err, never a silent None.
        let mut bad = summary(None);
        bad.mls_group_id = Some("not-valid-hex-zz".into());
        assert!(custody_channel_for(&bad, &c).is_err());
    }

    #[test]
    fn retired_serve_custody_finds_a_disabled_sets_rotated_out_generation() {
        // Served, now disabled: `resolve_engine_key_binding` already, correctly,
        // resolves this to `Unbound` — but the owner's custody still holds the
        // generation a served window sealed chunks under (`serve_disable`
        // rotates rather than forgets).
        let mut c = cfg();
        let mut disabled = summary(None); // mls_group_id: None, webdav_enabled: false
        disabled.name = "was-served".into();
        record_new_set(
            &mut c,
            fauna_core::folder_keys::serve_custody_channel_id("was-served"),
            [0x42; 32],
            1_000,
        );
        assert_eq!(
            retired_serve_custody(&c, &disabled).unwrap().current_key(),
            &[0x42; 32]
        );
        assert_eq!(
            resolve_engine_key_binding(&c, &disabled).unwrap(),
            EngineKeyBinding::Unbound,
            "the live binding must stay Unbound — retired custody never re-trips it"
        );
    }

    /// The pushed-blob producer must carry the same retired
    /// generation `retired_serve_custody` resolves directly — this is the
    /// wire-shaped twin of `retired_serve_custody_finds_a_disabled_sets_rotated_out_generation`,
    /// proving the field actually reaches `FolderEngineKeys` rather than just
    /// existing as a standalone free function.
    #[test]
    fn resolve_bindings_carries_the_retired_generation_for_a_disabled_served_set() {
        let mut c = cfg();
        let mut disabled = summary(None); // mls_group_id: None, webdav_enabled: false
        disabled.name = "was-served".into();
        record_new_set(
            &mut c,
            fauna_core::folder_keys::serve_custody_channel_id("was-served"),
            [0x42; 32],
            1_000,
        );
        let bindings = resolve_engine_key_bindings(&c, OWN, &[disabled], &[]).unwrap();
        assert_eq!(
            bindings.len(),
            1,
            "an unbound-but-once-served set is not omitted"
        );
        let entry = &bindings[0];
        assert_eq!(entry.mls_group_id, None, "live binding stays owner-only");
        assert_eq!(entry.content_keys, None, "live binding stays owner-only");
        assert_eq!(
            entry
                .retired_content_keys
                .as_ref()
                .map(FolderContentKeys::current_key),
            Some(&[0x42; 32]),
            "the retired serve-era generation must ride along for the bearer-only consumer"
        );
    }

    /// A set with no serve history at all must not carry a spurious `Some` —
    /// the common-case fleet pays nothing beyond the two summary-field checks
    /// `retired_serve_custody` documents.
    #[test]
    fn resolve_bindings_carries_no_retired_generation_for_a_never_served_set() {
        let c = cfg();
        let bindings = resolve_engine_key_bindings(&c, OWN, &[summary(None)], &[]).unwrap();
        assert_eq!(bindings[0].retired_content_keys, None);
    }

    #[test]
    fn retired_serve_custody_is_none_for_a_set_that_was_never_served() {
        let c = cfg();
        assert_eq!(retired_serve_custody(&c, &summary(None)), None);
    }

    #[test]
    fn retired_serve_custody_is_none_for_a_currently_served_or_bound_set() {
        let mut c = cfg();
        // Custody exists at BOTH identities — proves the answer turns on the
        // summary's OWN state, not on whether custody happens to be present.
        record_new_set(
            &mut c,
            fauna_core::folder_keys::serve_custody_channel_id("still-served"),
            [0x11; 32],
            1_000,
        );
        serve(&mut c, "still-served");
        assert_eq!(
            retired_serve_custody(&c, &served_summary("still-served")),
            None,
            "still served — the LIVE resolver owns this set's keys"
        );

        let raw = b"raw-openmls-group-id-bytes".to_vec();
        let channel_id = channel_id_for_group(&raw);
        record_new_set(&mut c, channel_id, [0x22; 32], 1_000);
        assert_eq!(
            retired_serve_custody(&c, &summary(Some(raw))),
            None,
            "bound — a shared set's content keys are never 'retired serve custody'"
        );
    }

    /// A custody that calls `name` served and holds no generation for it yet
    /// (the stamp joined before the keys did).
    fn served_keyless(c: &mut FoldersConfig, name: &str) {
        c.sets.push(fauna_core::data::FolderKeyCustody {
            channel_id: Some(fauna_core::folder_keys::serve_custody_channel_id(name)),
            served_at: Some(2_000),
            ..Default::default()
        });
    }

    #[test]
    fn resolve_bindings_omits_a_served_keyless_set_rather_than_erring() {
        // The capability batch must never ship a served set degradable to the
        // owner path — but a single served-keyless set now withholds only
        // ITS OWN engine (by omission), not the rest of the fleet's.
        let mut c = cfg();
        served_keyless(&mut c, "served-docs");
        let bindings = resolve_engine_key_bindings(
            &c,
            OWN,
            std::slice::from_ref(&served_summary("served-docs")),
            &[],
        )
        .expect("omission, not an error");
        assert!(
            bindings.is_empty(),
            "the keyless set is omitted, not shipped"
        );
    }

    #[test]
    fn resolve_bindings_omits_only_the_keyless_set_and_still_resolves_the_rest() {
        // A batch with one resolvable set and one served-but-keyless set must
        // ship the resolvable one — the whole point of narrowing the blast
        // radius from the batch to the one set that cannot be keyed.
        let mut c = cfg();
        record_new_set(
            &mut c,
            channel_id_for_group(b"raw-openmls-group-id-bytes"),
            [0x42; 32],
            1_000,
        );
        let mut s_bound = summary(Some(b"raw-openmls-group-id-bytes".to_vec()));
        s_bound.name = "shared-docs".into();
        let s_keyless = served_summary("served-docs");
        served_keyless(&mut c, "served-docs");

        let bindings = resolve_engine_key_bindings(&c, OWN, &[s_bound, s_keyless], &[])
            .expect("the batch resolves despite the keyless set");
        assert_eq!(
            bindings.len(),
            1,
            "only the resolvable set ships; the keyless one is omitted"
        );
        assert_eq!(bindings[0].folder, "shared-docs");
        assert_eq!(
            bindings[0].content_keys.as_ref().unwrap().current_key(),
            &[0x42; 32]
        );
    }

    #[test]
    fn resolve_bindings_carries_a_served_unshared_set_group_less() {
        let mut c = cfg();
        record_new_set(
            &mut c,
            fauna_core::folder_keys::serve_custody_channel_id("served-docs"),
            [0x42; 32],
            1_000,
        );
        serve(&mut c, "served-docs");
        let bindings = resolve_engine_key_bindings(
            &c,
            OWN,
            std::slice::from_ref(&served_summary("served-docs")),
            &[],
        )
        .unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].folder, "served-docs");
        assert_eq!(bindings[0].mls_group_id, None);
        assert_eq!(
            bindings[0].content_keys.as_ref().unwrap().current_key(),
            &[0x42; 32]
        );
    }

    /// A genesis `FolderContentKeys` with `key = [byte; 32]` at t=1000 — the
    /// shape `record_new_set` writes in these tests.
    fn _keys(byte: u8) -> FolderContentKeys {
        FolderContentKeys::genesis([byte; 32], 1_000)
    }

    #[test]
    fn malformed_hex_group_id_errs_rather_than_unbinding() {
        let c = cfg();
        let mut s = summary(None);
        s.mls_group_id = Some("not-valid-hex-zz".into());
        assert!(resolve_engine_key_binding(&c, &s).is_err());
    }

    #[test]
    fn resolve_bindings_tags_each_set_and_preserves_fail_closed() {
        // One bound set the owner holds keys for; one bound set with NO custody
        // (removed member); one owner-only set — the three arms, in one batch.
        let raw_bound = b"raw-openmls-group-id-bytes".to_vec();
        let channel_id = channel_id_for_group(&raw_bound);
        let mut c = cfg();
        record_new_set(&mut c, channel_id, [0x42; 32], 1_000);
        let raw_keyless = b"a-bound-set-with-no-keys-held".to_vec();

        let mut s_unbound = summary(None);
        s_unbound.name = "owner-only".into();
        let mut s_bound = summary(Some(raw_bound.clone()));
        s_bound.name = "shared-docs".into();
        let mut s_keyless = summary(Some(raw_keyless.clone()));
        s_keyless.name = "removed-member-set".into();

        let bindings =
            resolve_engine_key_bindings(&c, OWN, &[s_unbound, s_bound, s_keyless], &[]).unwrap();
        assert_eq!(bindings.len(), 3);

        // Unbound → (None, None).
        assert_eq!(bindings[0].folder, "owner-only");
        assert_eq!(bindings[0].mls_group_id, None);
        assert!(bindings[0].content_keys.is_none());

        // Bound-with-custody → (Some(raw), Some(keys)).
        assert_eq!(bindings[1].folder, "shared-docs");
        assert_eq!(bindings[1].mls_group_id, Some(raw_bound));
        assert_eq!(
            bindings[1].content_keys.as_ref().unwrap().current_key(),
            &[0x42; 32]
        );

        // Bound-but-keyless → (Some(raw), None) — fail-closed, NOT unbound.
        assert_eq!(bindings[2].folder, "removed-member-set");
        assert_eq!(bindings[2].mls_group_id, Some(raw_keyless));
        assert!(bindings[2].content_keys.is_none());
    }

    #[test]
    fn resolve_bindings_errs_on_any_malformed_hex() {
        let c = cfg();
        let mut bad = summary(None);
        bad.mls_group_id = Some("not-valid-hex-zz".into());
        assert!(resolve_engine_key_bindings(&c, OWN, std::slice::from_ref(&bad), &[]).is_err());
    }

    // ── Cross-nest (foreign) resolution ──

    /// Record a foreign set exactly as `record_foreign_set` does at share-accept.
    fn record_foreign(
        c: &mut FoldersConfig,
        name: Option<&str>,
        raw_group_id: &[u8],
        home: &str,
    ) -> [u8; 32] {
        let channel_id = channel_id_for_group(raw_group_id);
        c.foreign_sets.push(fauna_core::data::ForeignFolder {
            channel_id,
            mls_group_id: raw_group_id.to_vec(),
            home_nest_url: home.into(),
            home_nest_actor_id: None,
            set_name: name.map(str::to_string),
            access: Some("writer".into()),
            content_key_floor: None,
            ..Default::default()
        });
        channel_id
    }

    /// The whole point of the foreign resolver: a cross-nest set is in **no** nest
    /// projection, so before this existed the agent found no entry, ran the engine
    /// **unbound**, and sealed the member's edits under their own `BackupKey`. It
    /// must come back bound, keyed, and carrying its routing pair.
    #[test]
    fn a_foreign_set_resolves_bound_keyed_and_routed() {
        let raw = b"foreign-raw-group-id".to_vec();
        let mut c = cfg();
        let channel_id = record_foreign(&mut c, Some("xnest-docs"), &raw, "https://home.example");
        record_new_set(&mut c, channel_id, [0x77; 32], 1_000);

        let out = resolve_foreign_engine_key_bindings(&c);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].folder, "xnest-docs");
        assert_eq!(out[0].mls_group_id, Some(raw));
        assert_eq!(
            out[0].content_keys.as_ref().unwrap().current_key(),
            &[0x77; 32],
            "the member's own custody copy is what the engine seals + opens under"
        );
        assert_eq!(
            out[0].foreign_routing(),
            Some(("https://home.example".to_string(), hex::encode(channel_id))),
            "without the routing pair the agent builds a same-nest engine and the \
             record dies at the member's own nest with not_found"
        );
    }

    /// A foreign set whose custody has not arrived yet must be **bound-but-keyless**
    /// — `Some(gid)` + `None` — so the engine fails closed at
    /// `content_seal_root`. `(None, None)` would be the silent wrong-key path.
    #[test]
    fn a_foreign_set_without_custody_is_bound_keyless_not_unbound() {
        let raw = b"foreign-no-custody".to_vec();
        let mut c = cfg();
        record_foreign(&mut c, Some("xnest-docs"), &raw, "https://home.example");

        let out = resolve_foreign_engine_key_bindings(&c);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].mls_group_id,
            Some(raw),
            "the bound-marker must survive so the engine refuses rather than \
             degrading to the owner-only seal path"
        );
        assert!(out[0].content_keys.is_none());
    }

    /// A name-less record (an unopenable seal) is unaddressable by the
    /// name-keyed agent seam: emitting it as `""` would collide with every other
    /// name-less record. Skipping leaves it unbindable — reads still work by
    /// channel — which is the fail-safe direction.
    #[test]
    fn a_nameless_foreign_record_is_skipped_not_emitted_blank() {
        let raw = b"nameless-foreign".to_vec();
        let mut c = cfg();
        let channel_id = record_foreign(&mut c, None, &raw, "https://home.example");
        record_new_set(&mut c, channel_id, [0x33; 32], 1_000);

        assert!(
            resolve_foreign_engine_key_bindings(&c).is_empty(),
            "a name-less foreign record must not reach the blob at all"
        );
    }

    /// Access is advisory-for-UI only (D2), so it must NOT gate key material: a
    /// stale `reader` on a still-granted writer would silently unbind a live
    /// engine. Enforcement is the home nest's, at the mint.
    #[test]
    fn a_reader_grant_still_resolves_keys_because_access_is_not_authz() {
        let raw = b"foreign-reader".to_vec();
        let mut c = cfg();
        let channel_id = record_foreign(&mut c, Some("xnest-docs"), &raw, "https://home.example");
        c.foreign_sets[0].access = Some("reader".into());
        record_new_set(&mut c, channel_id, [0x55; 32], 1_000);

        let out = resolve_foreign_engine_key_bindings(&c);
        assert_eq!(out.len(), 1, "the entry is present regardless of the grant");
        assert!(out[0].content_keys.is_some());
    }

    // ── the pushed capability adopts the verifier ──────

    /// The owner whose custody `cfg()` is — the seat's own identity, and so
    /// the trusted owner of every row it owns.
    fn holder() -> fauna_core::identity::ActorKeypair {
        // The resolver's anchor is this keypair's actor id, so a genuine
        // attestation verifies.
        fauna_core::identity::ActorKeypair::from_secret([7u8; 32])
    }

    fn holder_cfg() -> FoldersConfig {
        FoldersConfig::default()
    }

    fn attested_public(summary: &mut FolderSummary, signer: &fauna_core::identity::ActorKeypair) {
        summary.audience = fauna_protocol::folders::AUDIENCE_PUBLIC.into();
        summary.audience_attestation = Some(fauna_protocol::folders::AudienceAttestation::mint(
            signer,
            summary.id,
            &summary.name,
            1_000,
            None,
        ));
    }

    /// The blob's `public_audience` is the verifier's verdict, not the nest's
    /// claim: a row the nest reports `public` with no attestation, or with one
    /// signed by anyone but the holder, ships **sealed**; only the holder's own
    /// genuine attestation over this row's id and name arms it.
    #[test]
    fn the_blobs_audience_flag_is_the_verifiers_verdict_not_the_nests_claim() {
        let c = holder_cfg();

        let mut bare = summary(None);
        bare.audience = fauna_protocol::folders::AUDIENCE_PUBLIC.into();
        let out = resolve_engine_key_bindings(&c, holder().actor_id(), &[bare], &[]).unwrap();
        assert!(
            !out[0].public_audience,
            "a bare `public` claim ships sealed"
        );

        let mut forged = summary(None);
        attested_public(
            &mut forged,
            &fauna_core::identity::ActorKeypair::from_secret([9u8; 32]),
        );
        let out = resolve_engine_key_bindings(&c, holder().actor_id(), &[forged], &[]).unwrap();
        assert!(
            !out[0].public_audience,
            "a stranger's signature ships sealed"
        );

        let mut genuine = summary(None);
        attested_public(&mut genuine, &holder());
        let out = resolve_engine_key_bindings(&c, holder().actor_id(), &[genuine], &[]).unwrap();
        assert!(out[0].public_audience, "the holder's own attestation arms");

        // Lent across rows: the same genuine attestation on a row wearing
        // another id fails as a bad signature (the id is signed).
        let mut lent = summary(None);
        attested_public(&mut lent, &holder());
        lent.id = 2;
        let out = resolve_engine_key_bindings(&c, holder().actor_id(), &[lent], &[]).unwrap();
        assert!(
            !out[0].public_audience,
            "an attestation for folder 1 arms nothing on folder 2"
        );
    }

    /// This producer runs on MLS-less hosts, so a **member** row has no anchor
    /// here, so the blob ships sealed — even
    /// when the nest attaches an attestation signed by the identity it fills
    /// into the row's owner field, which is exactly the identity a seat must
    /// never trust on the nest's word.
    #[test]
    fn a_member_row_has_no_anchor_on_an_mls_less_host_and_ships_sealed() {
        let raw = b"member-row-group".to_vec();
        let channel_id = channel_id_for_group(&raw);
        let mut c = holder_cfg();
        record_new_set(&mut c, channel_id, [0x42; 32], 1_000);
        let sharer = fauna_core::identity::ActorKeypair::from_secret([3u8; 32]);

        let mut member = summary(Some(raw));
        member.role = Some("member".into());
        member.owner_actor_id = Some(sharer.actor_id().to_hex());
        attested_public(&mut member, &sharer);

        let out = resolve_engine_key_bindings(&c, holder().actor_id(), &[member], &[]).unwrap();
        assert_eq!(out.len(), 1, "the member set still resolves its keys");
        assert!(out[0].content_keys.is_some());
        assert!(
            !out[0].public_audience,
            "no anchor ⇒ sealed, whatever the nest attaches"
        );
    }

    /// The anchor's rule, spelled once for every reader: an owned row is the
    /// seat's own id; a member row is the channel's recorded owner where the
    /// host holds MLS state, and nothing where it does not or never stamped.
    #[test]
    fn the_anchor_resolves_owned_rows_to_self_and_member_rows_through_the_marker() {
        struct Marked([u8; 32], ActorId);
        impl FolderChannelOwners for Marked {
            fn folder_channel_owner(&self, channel_id: &[u8; 32]) -> Option<ActorId> {
                (*channel_id == self.0).then_some(self.1)
            }
        }
        let own = ActorId([1u8; 32]);
        let recorded = ActorId([2u8; 32]);
        let raw = b"anchored-group".to_vec();
        let marked = Marked(channel_id_for_group(&raw), recorded);
        let anchor = DeclassificationAnchor {
            own,
            channel_owners: Some(&marked),
        };

        let owned = summary(None);
        assert_eq!(anchor.trusted_owner_for(&owned), Some(own));

        let mut member = summary(Some(raw));
        member.role = Some("member".into());
        member.owner_actor_id = Some(ActorId([9u8; 32]).to_hex());
        assert_eq!(
            anchor.trusted_owner_for(&member),
            Some(recorded),
            "the marker, never the nest-filled owner field"
        );

        let mut unstamped = summary(Some(b"never-stamped".to_vec()));
        unstamped.role = Some("member".into());
        assert_eq!(anchor.trusted_owner_for(&unstamped), None);

        assert_eq!(
            DeclassificationAnchor::own_only(own).trusted_owner_for(&member),
            None,
            "an MLS-less host has no member anchor"
        );
    }

    /// An engine host names a sealed row from what it already holds: its own
    /// row from custody by hash, a member's row by opening the seal under the
    /// set's content keys — and drops a row it can name by neither.
    #[test]
    fn an_engine_host_names_sealed_rows_from_custody_and_content_keys() {
        const GROUP: [u8; 20] = [0x7c; 20];
        let scrubbed = |id: i64, name: &str, sealed: Vec<u8>| FolderSummary {
            id,
            name: String::new(),
            name_hash: Some(fauna_protocol::ByteBuf::from(
                fauna_core::path_crypto::set_name_hash(name).to_vec(),
            )),
            name_sealed: Some(fauna_protocol::ByteBuf::from(sealed)),
            ..Default::default()
        };
        let mut cfg = FoldersConfig::default();
        // The holder's own set: custody carries its name.
        crate::custody::record_created_set(&mut cfg, "mine", [0x11; 32], None, 1_000);
        // A set shared with the holder: custody carries its content keys only.
        let channel = fauna_core::folder_keys::channel_id_for_group(&GROUP);
        crate::custody::record_new_set(&mut cfg, channel, [0x42; 32], 1_000);
        let content_root = fauna_core::path_crypto::LabelRoot::content_key([0x42; 32], 1);
        let seal = |root: &fauna_core::path_crypto::LabelRoot, name: &str| {
            fauna_core::label_custody::seal_set_name(root, name)
                .unwrap()
                .unwrap()
        };

        let mut theirs = scrubbed(2, "theirs", seal(&content_root, "theirs"));
        theirs.role = Some("member".into());
        theirs.mls_group_id = Some(hex::encode(GROUP));
        // Same group, but the name still rests under the owner's root — the
        // share's stamp has not landed, so no key this member holds opens it.
        let owner_root = fauna_core::path_crypto::LabelRoot::owner([0x99; 32]);
        let mut unstamped = scrubbed(3, "unstamped", seal(&owner_root, "unstamped"));
        unstamped.role = Some("member".into());
        unstamped.mls_group_id = Some(hex::encode(GROUP));

        let named = named_for_engine_host(
            vec![
                scrubbed(1, "mine", vec![1u8]),
                theirs,
                unstamped,
                scrubbed(4, "not-in-custody", vec![1u8]),
            ],
            &cfg,
        );
        let got: Vec<(i64, &str)> = named.iter().map(|r| (r.id, r.name.as_str())).collect();
        assert_eq!(got, vec![(1, "mine"), (2, "theirs")]);
    }
}
