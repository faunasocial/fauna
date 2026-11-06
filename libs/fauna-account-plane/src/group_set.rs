//! The **group set's readers** — the one place a member's group generation
//! keys, the set's content keys and its folded file rows are read from a
//! scope's persisted rows (`p2p-shared-set-build.md` § Build design — the
//! group set, decisions 3, 4 and 5).
//!
//! Beside [`crate::group_scope_view`], and built on its fold
//! ([`read_group_scope`]): the roster and the authority line come from there,
//! so "who is enrolled" has one answer whether a page lists the scope, the
//! serve door admits a peer, or a reader honours a file row.
//!
//! # What is here
//!
//! - [`held_group_generations`] — the tip and every retained generation this
//!   member holds a key for, opened from the merged mint and top-up rows with
//!   the member's own group-reception secrets. The group plane's
//!   `GenerationTip` seal and open arms key through it
//!   ([`crate::group_state_plane::GroupStatePlane`]).
//! - [`group_set_keys`] — those generations as the set's **content keys**
//!   (`fauna_core::group_content::group_file_content_key`), keyed by
//!   generation id: the ring the engine seals manifests and chunks under.
//!   No `u64` version exists on this family, ever.
//! - [`fold_group_files`] — the scope's merged `fauna.group.file` rows under
//!   the writer check; a row failing any arm is counted, never folded.
//! - [`fold_group_name`] — the scope's `fauna.group.name` cell under the
//!   name's writer check: only the authority's current devices name a set.

use std::collections::BTreeMap;
use std::sync::Mutex;

use anyhow::Result;
use fauna_account_store::{backend::StoreBackend, store::AccountStore, types::StateEntry};
use fauna_core::crypto::GenerationKey;
use fauna_core::encoding::canonical_decode;
use fauna_core::group_endpoints::{HonouredGroupEndpoints, verify_group_endpoints};
use fauna_core::group_file::{
    GroupFileChange, GroupGenerationEnrolment, HonouredGroupFile, group_generation_enrolment,
    verify_group_file,
};
use fauna_core::group_generation::{
    GroupGenerationMintRecord, GroupMemberWrap, GroupMintCore, GroupReceptionKeyRecord,
    GroupTopupRecord, group_generation_id, parse_group_topup_cell_key,
    resolve_admissible_group_tip, verify_group_mint_authorship,
};
use fauna_core::group_name::{HonouredGroupName, verify_group_name};
use fauna_mls::wrapped_blob::group_generation_wraps::open_group_generation_key_as_entry;
use fauna_protocol::group_state::{
    GROUP_NAME_KEY, KIND_GROUP_FILE, KIND_GROUP_GENERATION_MINT, KIND_GROUP_GENERATION_WRAP,
    KIND_GROUP_NAME, KIND_GROUP_ROSTER,
};
use fauna_protocol::merge_policy::{KIND_GROUP_RECEPTION_KEY, LwwStamp};
use fauna_protocol::scope::GroupScope;

use crate::group_scope_view::{GroupScopeLine, read_group_scope};
use crate::group_state_plane::reception_keys_from_rows;

/// The group generations this member holds a key for, in one scope.
#[derive(Default)]
pub struct GroupGenerationKeys {
    /// The current admissible, keyable tip — what new content seals under.
    /// `None` is the fail-safe gap: a `GenerationTip` write refuses.
    tip: Option<[u8; 32]>,
    keys: BTreeMap<[u8; 32], GenerationKey>,
}

impl GroupGenerationKeys {
    /// The tip's id and key, when one resolves.
    #[must_use]
    pub fn tip(&self) -> Option<([u8; 32], &GenerationKey)> {
        let id = self.tip?;
        self.keys.get(&id).map(|k| (id, k))
    }

    /// The key for `generation`, when this member holds one.
    #[must_use]
    pub fn key(&self, generation: &[u8; 32]) -> Option<&GenerationKey> {
        self.keys.get(generation)
    }

    /// The tip's id when one resolves — whether or not this member holds its
    /// key (the mirror carries the resolution as the plane read it).
    #[must_use]
    pub fn tip_id(&self) -> Option<[u8; 32]> {
        self.tip
    }

    /// Every (generation, key) this member holds, ascending by id — the ring
    /// as the app-dead mirror carries it (`group-set.md` § Build design — the
    /// group set, decision 12), the one reader beside the content-key fold.
    pub fn held(&self) -> impl Iterator<Item = (&[u8; 32], &GenerationKey)> {
        self.keys.iter()
    }
}

/// Open every generation of one scope this member can key, from the scope's
/// rows and the member's group-reception keys — pure, so the plane and the
/// store-reading door below agree by construction.
///
/// A generation counts only when its mint is **authored**
/// (`verify_group_mint_authorship` — the resolver's own check): a
/// machinery-root holder can craft a mint around a key it chose, and content
/// sealed under that key must open for nobody. Wraps are opened from the
/// mint's inline set and from every top-up at the generation; each open
/// verifies the recovered key against the mint's commitment.
#[must_use]
pub fn held_group_generations(
    scope_id: &[u8; 32],
    rows: &[StateEntry],
    reception: &[GroupReceptionKeyRecord],
) -> GroupGenerationKeys {
    let Some(line) = read_group_scope(scope_id, rows) else {
        return GroupGenerationKeys::default();
    };
    let secrets: Vec<_> = reception
        .iter()
        .filter_map(|r| r.keypair().ok().map(|k| k.secret))
        .collect();
    let topups: Vec<(&str, &[u8])> = live_rows(rows, KIND_GROUP_GENERATION_WRAP).collect();
    let open = |id: &[u8; 32], core: &GroupMintCore, wraps: &[GroupMemberWrap]| {
        let inline = wraps.iter().map(|w| (w.entry_id, w.wrap.clone()));
        let topped = topups.iter().filter_map(|(key, value)| {
            let (generation, target, _) = parse_group_topup_cell_key(key)?;
            let GroupTopupRecord::Wrap { wrap, .. } = canonical_decode(value).ok()?;
            (generation == *id).then_some((target, wrap))
        });
        inline.chain(topped).find_map(|(entry, wrap)| {
            secrets.iter().find_map(|secret| {
                open_group_generation_key_as_entry(&wrap, secret, id, &entry, &core.key_commitment)
                    .ok()
            })
        })
    };

    // Every authored generation this member can open — the retained set.
    let mut keys = BTreeMap::new();
    for (key, value) in live_rows(rows, KIND_GROUP_GENERATION_MINT) {
        let Ok(GroupGenerationMintRecord::Minted {
            core,
            minter_sig,
            authorization,
            wraps,
        }) = canonical_decode(value)
        else {
            continue;
        };
        let Ok(id) = group_generation_id(&core) else {
            continue;
        };
        if fauna_core::hex32::encode(&id) != key
            || verify_group_mint_authorship(
                &id,
                &core,
                &minter_sig,
                &authorization,
                &line.authority,
            )
            .is_err()
        {
            continue;
        }
        if let Some(gen_key) = open(&id, &core, &wraps) {
            keys.insert(id, gen_key);
        }
    }
    // The tip: the resolver's admissibility, keyable = opened above.
    let resolution = resolve_admissible_group_tip(
        &line.roster,
        &line.authority,
        live_rows(rows, KIND_GROUP_GENERATION_MINT),
        topups.iter().copied(),
        |id, _, _| keys.contains_key(id),
    );
    GroupGenerationKeys {
        tip: resolution.tip.map(|t| t.generation_id),
        keys,
    }
}

/// Read a scope's generations from `store`: its group rows and this
/// account's group-reception keys (`fauna.state.group-reception-key`, the
/// fleet-scope custody kind).
///
/// # Errors
/// A store read failing.
pub async fn read_held_group_generations<B: StoreBackend>(
    store: &AccountStore<B>,
    scope_id: &[u8; 32],
) -> Result<GroupGenerationKeys> {
    let rows = store
        .group_scope_states(&GroupScope::new(*scope_id).to_string())
        .await?;
    let reception = reception_keys_from_rows(store.states_of_kind(KIND_GROUP_RECEPTION_KEY).await?);
    Ok(held_group_generations(scope_id, &rows, &reception))
}

/// A group set's content keys (decision 4) — the type lives in
/// `fauna_core::group_content` so the download keys and the engine can carry
/// it; [`group_set_keys`] is still its one reader.
pub use fauna_core::group_content::GroupContentKeys;

impl From<&GroupGenerationKeys> for GroupContentKeys {
    fn from(held: &GroupGenerationKeys) -> Self {
        GroupContentKeys::from_generation_keys(held.tip, held.keys.iter())
    }
}

/// **The one reader of a group set's content keys** — roster, authority
/// line, tip resolution and wrap opening in one place; the share glue, the
/// engine host and the on-demand hosts all read it and none re-derives.
///
/// # Errors
/// A store read failing.
pub async fn group_set_keys<B: StoreBackend>(
    store: &AccountStore<B>,
    scope_id: &[u8; 32],
) -> Result<GroupContentKeys> {
    Ok(GroupContentKeys::from(
        &read_held_group_generations(store, scope_id).await?,
    ))
}

/// One path as the lift hands it to the plane (decision 7): the engine's
/// pending own row, in the file record's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupFileDraft {
    /// The normalized folder-relative path, plaintext — sealed here, never
    /// carried onward in the clear.
    pub path: String,
    pub change: GroupFileChange,
    /// The generation whose content key sealed the body — the pending row's
    /// stamp, or for a delete (which seals nothing) the ring's tip.
    pub generation_id: [u8; 32],
    /// The author's instant, unix ms — the pending row's own `created_at`,
    /// so a re-lift of the same row signs and stamps byte-identically.
    pub stamped_at_ms: i64,
    /// The head the edit wrote over (decision 10) — the pending row's own
    /// `base_manifest`, carried into the signed core verbatim.
    pub base_manifest: Option<[u8; 32]>,
}

/// **The lift's write** (decision 7): build, sign and put one
/// `fauna.group.file` row for `draft` as this device — its path hashed
/// under the scope ([`fauna_core::group_file::group_file_path_hash`]) and
/// sealed under the named generation's content key, its core carrying this
/// member's roster entry core so a reader's writer check can enrol it,
/// signed by `writer_key` carrying `authorization` (the device's
/// `DeviceAuthorization` carriage), and stamped with the author's own
/// instant and device. Answers the writer seq the entry landed on — the
/// `Ok(seq)` the engine's pending row is upgraded with.
///
/// # Errors
/// The ring holds no key for the draft's generation; the carriage does not
/// verify or certifies another device; or the plane's put refuses.
pub async fn put_group_file<B: StoreBackend>(
    store: &AccountStore<B>,
    root: &fauna_core::crypto::GroupMachineryRoot,
    writer_key: &ed25519_dalek::SigningKey,
    scope_id: &[u8; 32],
    authorization: Vec<u8>,
    draft: &GroupFileDraft,
) -> Result<u64> {
    let device = writer_key.verifying_key().to_bytes();
    let cert = fauna_core::group_scope::verify_device_cert_carriage(
        "lifted file row authorization",
        &authorization,
        draft.stamped_at_ms,
    )
    .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        cert.device_key == device,
        "the device authorization certifies another device than the one signing the lift"
    );
    let rows = store
        .group_scope_states(&GroupScope::new(*scope_id).to_string())
        .await?;
    let reception = reception_keys_from_rows(store.states_of_kind(KIND_GROUP_RECEPTION_KEY).await?);
    let ring = GroupContentKeys::from(&held_group_generations(scope_id, &rows, &reception));
    let Some(content_key) = ring.key(&draft.generation_id) else {
        anyhow::bail!("this member holds no content key for the generation the lifted row names");
    };
    // The author's own roster entry core — `None` for an authority that
    // holds no entry, whose rows need no wrap to be honoured.
    let writer_entry = rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_ROSTER && !r.tombstone)
        .find_map(|r| match canonical_decode(&r.value).ok()? {
            fauna_core::group_scope::GroupRosterRecord::Enrolled { core, .. }
                if core.member_actor == cert.actor_id =>
            {
                Some(core)
            }
            _ => None,
        });
    let path_hash = fauna_core::group_file::group_file_path_hash(scope_id, &draft.path);
    let sealed_path = fauna_core::group_file::seal_group_file_path(
        content_key,
        &draft.generation_id,
        &path_hash,
        &draft.path,
    )
    .map_err(anyhow::Error::msg)?;
    let record = fauna_core::group_file::GroupFileRecord::sign(
        fauna_core::group_file::GroupFileCore {
            scope_id: *scope_id,
            path_hash,
            sealed_path,
            change: draft.change.clone(),
            generation_id: draft.generation_id,
            stamped_at_ms: draft.stamped_at_ms,
            writer_entry,
            base_manifest: draft.base_manifest,
        },
        writer_key,
        authorization,
    )?;
    let stamp = LwwStamp {
        at_ms: draft.stamped_at_ms,
        writer: device,
    }
    .encode()?;
    crate::group_state_plane::GroupStatePlane::new(
        store,
        &crate::group_state_plane::NoFeed,
        root,
        writer_key,
        scope_id,
    )?
    .put(
        &crate::account_state_plane::ItemId {
            kind: KIND_GROUP_FILE.into(),
            key: record.logical_key(),
        },
        fauna_core::encoding::canonical_encode(&record)?.to_vec(),
        Some(stamp),
    )
    .await
}

/// The writer check's inputs for one scope, read once from its rows — the
/// file rows' check and the name's, both off the one authority line.
pub struct GroupWriterCheck {
    scope_id: [u8; 32],
    line: GroupScopeLine,
    enrolment: GroupGenerationEnrolment,
}

impl GroupWriterCheck {
    /// `None` when this replica cannot say what the scope is (no birth row).
    #[must_use]
    pub fn from_rows(scope_id: &[u8; 32], rows: &[StateEntry]) -> Option<GroupWriterCheck> {
        let line = read_group_scope(scope_id, rows)?;
        let enrolment = group_generation_enrolment(
            &line.authority,
            &line.roster,
            live_rows(rows, KIND_GROUP_GENERATION_MINT),
            live_rows(rows, KIND_GROUP_GENERATION_WRAP),
        );
        Some(GroupWriterCheck {
            scope_id: *scope_id,
            line,
            enrolment,
        })
    }

    /// A file row: decision 5's three arms, plus the stamp the entry merges
    /// under: it must be the author's own signed instant and device, or a
    /// re-publisher could re-rank a member's signed value.
    ///
    /// # Errors
    /// The refusal reason.
    pub fn verify_file(
        &self,
        key: &str,
        value: &[u8],
        merge_meta: Option<&[u8]>,
    ) -> Result<HonouredGroupFile, String> {
        let honoured = verify_group_file(
            &self.scope_id,
            key,
            value,
            &self.line.authority,
            &self.line.roster,
            &self.enrolment,
        )?;
        check_author_stamp(
            "file row",
            merge_meta,
            honoured.record.core.stamped_at_ms,
            &honoured.device,
        )?;
        Ok(honoured)
    }

    /// The set's name (decision 2): its one cell, signed by a current
    /// authority device, merging under that device's signed instant.
    ///
    /// # Errors
    /// The refusal reason.
    pub fn verify_name(
        &self,
        key: &str,
        value: &[u8],
        merge_meta: Option<&[u8]>,
    ) -> Result<HonouredGroupName, String> {
        if key != GROUP_NAME_KEY {
            return Err(format!(
                "set name sits under {key:?}, not its one cell {GROUP_NAME_KEY:?}"
            ));
        }
        let honoured = verify_group_name(&self.scope_id, value, &self.line.authority)?;
        check_author_stamp(
            "set name",
            merge_meta,
            honoured.record.core.stamped_at_ms,
            &honoured.device,
        )?;
        Ok(honoured)
    }

    /// A member device's endpoints cell (decision 8): under its own key,
    /// signed by a current authority device or a live member's, naming that
    /// member's own actor, merging under that device's signed
    /// instant.
    ///
    /// # Errors
    /// The refusal reason.
    pub fn verify_endpoints(
        &self,
        key: &str,
        value: &[u8],
        merge_meta: Option<&[u8]>,
    ) -> Result<HonouredGroupEndpoints, String> {
        let honoured = verify_group_endpoints(
            &self.scope_id,
            key,
            value,
            &self.line.authority,
            &self.line.roster,
        )?;
        check_author_stamp(
            "endpoints cell",
            merge_meta,
            honoured.record.core.stamped_at_ms,
            &honoured.device,
        )?;
        Ok(honoured)
    }
}

/// The entry's `LwwStamp` must be the author's signed instant and device.
fn check_author_stamp(
    what: &str,
    merge_meta: Option<&[u8]>,
    signed_at_ms: i64,
    device: &[u8; 32],
) -> Result<(), String> {
    let stamp = merge_meta
        .ok_or_else(|| format!("{what} carries no stamp"))
        .and_then(|m| LwwStamp::decode(m).map_err(|_| format!("{what} stamp does not decode")))?;
    if stamp.at_ms != signed_at_ms || stamp.writer != *device {
        return Err(format!(
            "{what} stamp is not its author's signed instant and device"
        ));
    }
    Ok(())
}

/// The scope's file rows, folded.
#[derive(Debug, Default)]
pub struct GroupFileFold {
    /// Every honoured row, by logical key (the path hash hex).
    pub files: BTreeMap<String, HonouredGroupFile>,
    /// Rows the writer check refused — counted, never folded.
    pub refused: usize,
}

/// Fold a scope's merged `fauna.group.file` rows under the writer check.
/// `None` when this replica cannot say what the scope is.
#[must_use]
pub fn fold_group_files(scope_id: &[u8; 32], rows: &[StateEntry]) -> Option<GroupFileFold> {
    let check = GroupWriterCheck::from_rows(scope_id, rows)?;
    let mut fold = GroupFileFold::default();
    for row in rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_FILE && !r.tombstone)
    {
        match check.verify_file(&row.key, &row.value, row.merge_meta.as_deref()) {
            Ok(honoured) => {
                fold.files.insert(row.key.clone(), honoured);
            }
            Err(why) => {
                tracing::warn!(key = %row.key, "refusing a group file row: {why}");
                fold.refused += 1;
            }
        }
    }
    Some(fold)
}

/// The scope's name, folded.
#[derive(Debug, Default)]
pub struct GroupNameFold {
    /// The honoured name. `None` for a nameless set, which keeps its short
    /// id, or for a set whose stored name was refused.
    pub name: Option<HonouredGroupName>,
    /// Name rows the writer check refused — counted, never folded.
    pub refused: usize,
}

/// Fold a scope's merged `fauna.group.name` cell under the name's writer
/// check. This is the second line behind the walk's: it refuses a name a
/// member stored by sealing it itself, and it retires a name whose authority
/// device the line has since revoked. `None` when this replica cannot say
/// what the scope is.
#[must_use]
pub fn fold_group_name(scope_id: &[u8; 32], rows: &[StateEntry]) -> Option<GroupNameFold> {
    let check = GroupWriterCheck::from_rows(scope_id, rows)?;
    let mut fold = GroupNameFold::default();
    for row in rows
        .iter()
        .filter(|r| r.kind == KIND_GROUP_NAME && !r.tombstone)
    {
        match check.verify_name(&row.key, &row.value, row.merge_meta.as_deref()) {
            Ok(honoured) => fold.name = Some(honoured),
            Err(why) => {
                tracing::warn!(key = %row.key, "refusing a group name row: {why}");
                fold.refused += 1;
            }
        }
    }
    Some(fold)
}

/// A per-plane memo of one scope's keys and writer check, rebuilt from the
/// store whenever a lookup misses — a generation's mint or a member's
/// top-up may land mid-walk, and a miss is exactly when it matters.
#[derive(Default)]
pub(crate) struct ScopeReadCache(Mutex<Option<std::sync::Arc<ScopeRead>>>);

pub(crate) struct ScopeRead {
    pub(crate) generations: GroupGenerationKeys,
    pub(crate) check: Option<GroupWriterCheck>,
}

impl ScopeReadCache {
    pub(crate) fn current(&self) -> Option<std::sync::Arc<ScopeRead>> {
        self.0.lock().expect("scope read cache").clone()
    }

    /// Drop the memo: a row the writer check reads has landed, and a check
    /// can NARROW (a removal, a revocation), so a memo that still passes a
    /// row is no longer evidence the row passes.
    pub(crate) fn invalidate(&self) {
        *self.0.lock().expect("scope read cache") = None;
    }

    pub(crate) async fn refresh<B: StoreBackend>(
        &self,
        store: &AccountStore<B>,
        scope_id: &[u8; 32],
    ) -> Result<std::sync::Arc<ScopeRead>> {
        let rows = store
            .group_scope_states(&GroupScope::new(*scope_id).to_string())
            .await?;
        let reception =
            reception_keys_from_rows(store.states_of_kind(KIND_GROUP_RECEPTION_KEY).await?);
        let read = std::sync::Arc::new(ScopeRead {
            generations: held_group_generations(scope_id, &rows, &reception),
            check: GroupWriterCheck::from_rows(scope_id, &rows),
        });
        *self.0.lock().expect("scope read cache") = Some(read.clone());
        Ok(read)
    }
}

fn live_rows<'a>(
    rows: &'a [StateEntry],
    kind: &'a str,
) -> impl Iterator<Item = (&'a str, &'a [u8])> {
    rows.iter()
        .filter(move |r| r.kind == kind && !r.tombstone)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
}
