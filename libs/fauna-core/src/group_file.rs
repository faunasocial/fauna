//! **A group set's path state** — the `fauna.group.file` value and the
//! writer check every reader of it runs (`p2p-shared-set-build.md` § Build
//! design — the group set, decisions 2 and 5).
//!
//! A folder bound under a recipient-set group scope keeps its file rows as
//! class-2 entries of the scope itself: one `LatestWins` cell per path, its
//! logical key the set's path hash, its value a [`GroupFileRecord`]. The
//! entry is sealed under the generation tip (the group content branch,
//! [`crate::crypto::GroupContentSchedule`]), but the entry envelope's own
//! writer signature names whichever replica last *re-published* the row —
//! the group plane re-seals what it merges — so authorship rides inside the
//! value: the author device's `DeviceAuthorization` carriage and a signature
//! over the value core, the roster record's shape.
//!
//! # Who may write — enrolment at the row's generation, read off the wraps
//!
//! A reader honours a row iff (a) the carriage's cert chains the writer
//! device to an actor M, (b) M is the authority, or the generation the row
//! names carries a wrap for M's roster entry, (b′) when the reader's merged
//! roster excludes that entry, the row's signed stamp is not after the
//! earliest honoured removal of it, and (c) the signature verifies. Arm (b)
//! consults no stamp — stamps are advisory and the plane has no arbiter —
//! and what it buys is that a removed member's past files stay in the set:
//! it cannot obtain a post-removal generation key, so cannot seal under one,
//! and what it sealed while enrolled stays what it was. Arm (b′) is the
//! removal bound (`group-set.md` decision 5, the 2026-11-02 ruling, rule
//! (i)): enrolment at a generation is permanent, so without it a removed
//! member still holding a pre-removal generation could sign NEW rows naming
//! it, and one stale co-member merging them would re-serve them to every
//! replica for good. The bound is the authority's signed removal instant,
//! read off the roster as (b) reads the wraps — a pure function of merged
//! rows, never of a reader's clock: a removed member is frozen at its
//! removal wherever the removal has merged. Which honoured row a cell
//! folds to is the reader's per-writer fold (rule (ii)), not this check's.
//!
//! **The row carries its writer's roster entry core** (a refinement the
//! build made of decision 5): a roster cell's `Removed` phase absorbs the
//! `Enrolled` record it replaces, and with it the only copy of the core that
//! names the member's actor — so after a removal no reader could map the
//! wrapped entry id back to M from the roster. The writer's own core,
//! recomputed to its content-derived entry id, carries that binding with the
//! row; the id commits to the scope and the actor, so it cannot be borrowed.

use std::collections::{BTreeMap, BTreeSet};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::encoding::{canonical_decode, canonical_encode};
use crate::error::Error;
use crate::group_generation::{
    GroupGenerationMintRecord, GroupTopupRecord, group_generation_id, parse_group_topup_cell_key,
    verify_group_mint_authorship,
};
use crate::group_scope::{
    GroupAuthority, RosterEntryCore, RosterView, roster_entry_id, verify_device_cert_carriage,
};
use crate::identity::ActorId;

/// Domain-separation tag for a group file row's author signature. **Frozen**:
/// every row a member ever wrote verifies through it.
pub const GROUP_FILE_SIG_CONTEXT: &[u8] = b"fauna.group.file.sig.v1 2026-10-29\0";

/// What a file row says happened to its path. **A delete is a value phase,
/// never a tombstone**: the group plane has no tombstone door, and a deleted
/// path must stay a merged fact every replica folds alike.
///
/// Closed by design (`transport.md` § Schema and forward-compat discipline →
/// *Rule 3 in full*, the `consensus` ground): every reader must fold the same
/// value from the same bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupFileChange {
    /// The path holds the body this manifest names.
    Put {
        /// The sealed manifest's content address — what the share leg's
        /// `manifests.get` serves under the set id.
        #[serde(with = "serde_bytes")]
        manifest_hash: [u8; 32],
        /// The plaintext body's size, bytes.
        size_bytes: u64,
    },
    /// The path was deleted.
    Delete,
}

/// Everything a file row asserts — exactly what its author signs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupFileCore {
    /// The group scope the row belongs to. Signed, so a row is never
    /// replayed into another scope its author also writes.
    #[serde(with = "serde_bytes")]
    pub scope_id: [u8; 32],
    /// The set's keyed path hash (`crate::path_crypto`, keyed under the
    /// scope's content branch) — the row's logical key, echoed and signed so
    /// a re-publisher cannot re-file a signed value under another path.
    #[serde(with = "serde_bytes")]
    pub path_hash: [u8; 32],
    /// The path, sealed — names rest sealed (`file-sync.md` § Sealed names &
    /// paths).
    #[serde(with = "serde_bytes")]
    pub sealed_path: Vec<u8>,
    /// What happened to the path.
    pub change: GroupFileChange,
    /// The group generation whose derived content key sealed the body. **No
    /// `u64` version exists on this family and none may be synthesized**:
    /// generation ids are content-derived and only partially ordered.
    #[serde(with = "serde_bytes")]
    pub generation_id: [u8; 32],
    /// The author's clock, unix ms — the `LwwStamp` the entry merges under
    /// carries exactly this instant and the author device, so the order a
    /// replica merges by is the one the author signed. Advisory otherwise.
    pub stamped_at_ms: i64,
    /// The author's roster entry core (module docs) — `None` only for a row
    /// the scope's authority writes, which needs no wrap to be honoured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_entry: Option<RosterEntryCore>,
    /// The head the row wrote over (decision 10): the manifest its author's
    /// replica tracked for the path when the edit was made — `None` for a
    /// create over nothing or over a tracked delete; a delete names the
    /// manifest it deleted. Signed, so a relay can neither strip nor forge
    /// it; omitted when absent, so a row without it encodes as before. A
    /// reader whose own lifted head IS this base takes the row as an
    /// ordinary supersede rather than a concurrent loss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "serde_bytes")]
    pub base_manifest: Option<[u8; 32]>,
}

/// One `fauna.group.file` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupFileRecord {
    /// The signed assertion.
    pub core: GroupFileCore,
    /// The author device's `DeviceAuthorization`, in the `EmbedAsBytes`
    /// carriage every witness surface uses.
    #[serde(with = "serde_bytes")]
    pub authorization: Vec<u8>,
    /// Ed25519 by the authorized device over [`group_file_signing_bytes`].
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

/// The logical key of a path's cell — its path hash, hex.
#[must_use]
pub fn group_file_logical_key(path_hash: &[u8; 32]) -> String {
    crate::hex32::encode(path_hash)
}

/// Domain-separation context of a group set's keyed path hash. **Frozen**:
/// every cell a member ever wrote is keyed through it.
const GROUP_FILE_PATH_HASH_CONTEXT: &str = "fauna.group.file.path-hash.v1 2026-10-30";

/// Domain-separation context of a group set's path seal. **Frozen**.
const GROUP_FILE_PATH_SEAL_CONTEXT: &str = "fauna.group.file.path.v1 2026-10-30";

/// A group set's keyed path hash — the row's [`GroupFileCore::path_hash`]
/// and so its cell. Keyed by the SCOPE, never by a generation: a path keeps
/// one cell across every tip the scope mints, so a later generation's write
/// supersedes the earlier one in place rather than opening a second cell
/// for the same path. The key keeps one path's hash from linking two sets
/// it appears in; on the wire the cell's key rides blinded by the plane.
#[must_use]
pub fn group_file_path_hash(scope_id: &[u8; 32], normalized_rel_path: &str) -> [u8; 32] {
    let key = blake3::derive_key(GROUP_FILE_PATH_HASH_CONTEXT, scope_id);
    *blake3::keyed_hash(&key, normalized_rel_path.as_bytes()).as_bytes()
}

/// The path seal's AEAD key and nonce for one row: keyed by the generation's
/// content key, the nonce derived from the path hash — convergent, which is
/// legal because the path hash determines the path.
fn path_seal_parts(content_key: &[u8; 32], path_hash: &[u8; 32]) -> ([u8; 32], [u8; 12]) {
    let key =
        crate::domain_key::derive_domain_key(GROUP_FILE_PATH_SEAL_CONTEXT, content_key, b"key");
    let nonce_key =
        crate::domain_key::derive_domain_key(GROUP_FILE_PATH_SEAL_CONTEXT, content_key, b"nonce");
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&blake3::keyed_hash(&nonce_key, path_hash).as_bytes()[..12]);
    (key, nonce)
}

/// The associated data a sealed path binds: its generation and path hash,
/// so a sealed path copied onto another row or relabelled to another
/// generation does not open.
fn path_seal_aad(generation_id: &[u8; 32], path_hash: &[u8; 32]) -> [u8; 64] {
    let mut aad = [0u8; 64];
    aad[..32].copy_from_slice(generation_id);
    aad[32..].copy_from_slice(path_hash);
    aad
}

/// Seal a row's path under the content key of the generation the row names
/// ([`crate::group_content::group_file_content_key`]) — names rest sealed
/// (`file-sync.md` § Sealed names & paths) inside the value too, not only
/// under the entry's own seal.
///
/// # Errors
/// Only an AEAD failure, unreachable for a 32-byte key; never swallowed.
pub fn seal_group_file_path(
    content_key: &[u8; 32],
    generation_id: &[u8; 32],
    path_hash: &[u8; 32],
    path: &str,
) -> Result<Vec<u8>, String> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    let (key, nonce) = path_seal_parts(content_key, path_hash);
    chacha20poly1305::ChaCha20Poly1305::new((&key).into())
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: path.as_bytes(),
                aad: &path_seal_aad(generation_id, path_hash),
            },
        )
        .map_err(|e| format!("seal group file path: {e}"))
}

/// Open a row's sealed path and check it hashes to the row's own cell: a
/// path that opens but names another cell is refused, so a row can never
/// land under a path other than the one it is filed under.
///
/// # Errors
/// A wrong key, a tampered or relabelled body, a non-UTF-8 path, or a path
/// whose keyed hash is not `path_hash`. Not distinguished beyond that.
pub fn open_group_file_path(
    content_key: &[u8; 32],
    scope_id: &[u8; 32],
    generation_id: &[u8; 32],
    path_hash: &[u8; 32],
    sealed: &[u8],
) -> Result<String, String> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    let (key, nonce) = path_seal_parts(content_key, path_hash);
    let plain = chacha20poly1305::ChaCha20Poly1305::new((&key).into())
        .decrypt(
            (&nonce).into(),
            Payload {
                msg: sealed,
                aad: &path_seal_aad(generation_id, path_hash),
            },
        )
        .map_err(|_| "group file path does not open".to_string())?;
    let path = String::from_utf8(plain).map_err(|_| "group file path is not UTF-8".to_string())?;
    if group_file_path_hash(scope_id, &path) != *path_hash {
        return Err("group file path does not hash to its cell".into());
    }
    Ok(path)
}

/// The exact bytes a file row's author signs: the frozen tag, the BLAKE3 of
/// the canonical core, and the BLAKE3 of the carried authorization — the
/// cert is signed so a copy with it swapped no longer verifies (the shred
/// attestation's v2 lesson).
///
/// # Errors
/// Only on a core that fails canonical encoding.
pub fn group_file_signing_bytes(
    core: &GroupFileCore,
    authorization: &[u8],
) -> Result<Vec<u8>, Error> {
    let core_bytes = canonical_encode(core)?;
    let mut out = Vec::with_capacity(GROUP_FILE_SIG_CONTEXT.len() + 64);
    out.extend_from_slice(GROUP_FILE_SIG_CONTEXT);
    out.extend_from_slice(blake3::hash(&core_bytes).as_bytes());
    out.extend_from_slice(blake3::hash(authorization).as_bytes());
    Ok(out)
}

impl GroupFileRecord {
    /// Sign `core` as `device`, carrying `authorization` (its cert).
    ///
    /// # Errors
    /// Only on a core that fails canonical encoding.
    pub fn sign(
        core: GroupFileCore,
        device: &SigningKey,
        authorization: Vec<u8>,
    ) -> Result<GroupFileRecord, Error> {
        let signature = device
            .sign(&group_file_signing_bytes(&core, &authorization)?)
            .to_bytes()
            .to_vec();
        Ok(GroupFileRecord {
            core,
            authorization,
            signature,
        })
    }

    /// The cell this record must sit in.
    #[must_use]
    pub fn logical_key(&self) -> String {
        group_file_logical_key(&self.core.path_hash)
    }
}

/// Which roster entries each group generation enrols, as the merged rows
/// record it — the reader's half of arm (b). Built once per scope read from
/// the merged mint, top-up and roster rows; a pure function of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupGenerationEnrolment {
    wrapped: BTreeMap<[u8; 32], BTreeSet<[u8; 32]>>,
}

impl GroupGenerationEnrolment {
    /// Does `generation` carry a wrap for `entry`?
    #[must_use]
    pub fn enrols(&self, generation: &[u8; 32], entry: &[u8; 32]) -> bool {
        self.wrapped
            .get(generation)
            .is_some_and(|entries| entries.contains(entry))
    }
}

/// Fold merged rows into each generation's wrapped entry set.
///
/// - A **mint** counts only once authored: its row key is its core's
///   content-derived id and [`verify_group_mint_authorship`] admits it — the
///   resolver's own check, so a machinery-root holder cannot mint itself a
///   wrap. Every inline wrap's entry is enrolled at that generation. A
///   `Shredded` row carries no wraps and enrols nobody — its key is gone.
/// - A **top-up** counts when it verifies at its cell, its healer is a
///   verified member, and its target is an entry the authority enrolled
///   (enrolled now, or excluded by a verified removal) — the late-add arm:
///   a member admitted after a generation was minted reads and writes under
///   it through a top-up, never through the mint. Decision 5 names the mint
///   rows only; the build widened it to the coverage the resolver already
///   honours, or every later-added member's rows would be refused.
pub fn group_generation_enrolment<'a, M, T>(
    authority: &GroupAuthority,
    roster: &RosterView,
    mint_rows: M,
    topup_rows: T,
) -> GroupGenerationEnrolment
where
    M: IntoIterator<Item = (&'a str, &'a [u8])>,
    T: IntoIterator<Item = (&'a str, &'a [u8])>,
{
    let mut out = GroupGenerationEnrolment::default();
    for (key, value) in mint_rows {
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
        if crate::hex32::encode(&id) != key
            || verify_group_mint_authorship(&id, &core, &minter_sig, &authorization, authority)
                .is_err()
        {
            continue;
        }
        out.wrapped
            .entry(id)
            .or_default()
            .extend(wraps.iter().map(|w| w.entry_id));
    }
    for (key, value) in topup_rows {
        let Some((generation, target, healer)) = parse_group_topup_cell_key(key) else {
            continue;
        };
        let Ok(record) = canonical_decode::<GroupTopupRecord>(value) else {
            continue;
        };
        if record.verifies_at(&generation, &target, &healer)
            && roster.is_verified_member(&ActorId(healer))
            && (roster.is_enrolled_entry(&target) || roster.is_excluded_entry(&target))
        {
            out.wrapped.entry(generation).or_default().insert(target);
        }
    }
    out
}

/// A file row a reader honours, with the author its checks established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HonouredGroupFile {
    /// The verified record.
    pub record: GroupFileRecord,
    /// The actor arm (a) chained the writer device to.
    pub member: ActorId,
    /// The writer device — the `LwwStamp` writer the entry must merge under.
    pub device: [u8; 32],
}

/// **The writer check** (decision 5): decode `value` at cell `key` of scope
/// `scope_id` and honour it iff its carriage chains the writer device to an
/// actor M (arm (a)), M is the authority or `enrolment` enrols M's entry at
/// the row's generation (arm (b)), a member whose entry `roster` excludes
/// stamped the row no later than its earliest honoured removal (arm (b′),
/// the removal bound), and the signature verifies (arm (c)).
///
/// # Errors
/// The refusal reason — a row failing any arm is counted `refused` and never
/// folded.
pub fn verify_group_file(
    scope_id: &[u8; 32],
    key: &str,
    value: &[u8],
    authority: &GroupAuthority,
    roster: &RosterView,
    enrolment: &GroupGenerationEnrolment,
) -> Result<HonouredGroupFile, String> {
    let record: GroupFileRecord =
        canonical_decode(value).map_err(|e| format!("file row does not decode: {e}"))?;
    let core = &record.core;
    if core.scope_id != *scope_id {
        return Err("file row names another scope".into());
    }
    if record.logical_key() != key {
        return Err("file row sits under a key that is not its path hash".into());
    }
    // Arm (a): the carriage chains the device to an actor.
    let cert = verify_device_cert_carriage(
        "file row authorization",
        &record.authorization,
        core.stamped_at_ms,
    )?;
    // Arm (b): the authority, or enrolled at the row's generation.
    if cert.actor_id == *authority.root() {
        authority.verify_device_cert(
            "file row authorization",
            &record.authorization,
            core.stamped_at_ms,
        )?;
    } else {
        let entry_core = core
            .writer_entry
            .as_ref()
            .ok_or("a member's file row carries no roster entry core")?;
        if entry_core.scope_id != *scope_id || entry_core.member_actor != cert.actor_id {
            return Err("file row's entry core is not its writer's in this scope".into());
        }
        let entry = roster_entry_id(entry_core).map_err(|e| e.to_string())?;
        if !enrolment.enrols(&core.generation_id, &entry) {
            return Err("file row's writer is not enrolled at the generation it names".to_string());
        }
        // Arm (b′): a removed member is honoured as of its removal.
        if let Some(removed_at_ms) = roster.excluded_since(&entry)
            && core.stamped_at_ms > removed_at_ms
        {
            return Err(
                "file row's writer is removed and the row is stamped after the removal".into(),
            );
        }
    }
    // Arm (c): the signature.
    let signed =
        group_file_signing_bytes(core, &record.authorization).map_err(|e| e.to_string())?;
    if !crate::identity::verify_detached(&cert.device_key, &signed, &record.signature) {
        return Err("file row signature does not verify".into());
    }
    Ok(HonouredGroupFile {
        member: cert.actor_id,
        device: cert.device_key,
        record,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCOPE: [u8; 32] = [0x5c; 32];
    const GEN: [u8; 32] = [0x7a; 32];
    const KEY: [u8; 32] = [0x11; 32];

    /// Known-answer pin: changing the context or the keying re-files every
    /// cell a member ever wrote, so it is a data migration, not a refactor.
    #[test]
    fn the_path_hash_is_pinned_and_keyed_by_the_scope() {
        assert_eq!(
            crate::hex32::encode(&group_file_path_hash(&SCOPE, "photos/a.jpg")),
            "90e4f4727003294856585329a753b3883ff5309551be56f4bd77081637f81ae3"
        );
        assert_ne!(
            group_file_path_hash(&SCOPE, "photos/a.jpg"),
            group_file_path_hash(&[0x5d; 32], "photos/a.jpg"),
            "one path in two sets hashes apart"
        );
        assert_ne!(
            group_file_path_hash(&SCOPE, "photos/a.jpg"),
            crate::sync::path_hash("photos/a.jpg"),
            "never the unkeyed sync hash"
        );
    }

    fn core(base_manifest: Option<[u8; 32]>) -> GroupFileCore {
        GroupFileCore {
            scope_id: SCOPE,
            path_hash: group_file_path_hash(&SCOPE, "docs/plan.txt"),
            sealed_path: vec![1, 2, 3],
            change: GroupFileChange::Put {
                manifest_hash: [0x33; 32],
                size_bytes: 7,
            },
            generation_id: GEN,
            stamped_at_ms: 1_700_000_000_000,
            writer_entry: None,
            base_manifest,
        }
    }

    /// Decision 10: the base is additive inside the signed core — a core
    /// without it encodes byte-for-byte as the core did before the field
    /// existed, so every row written before still verifies.
    #[test]
    fn a_core_without_a_base_encodes_as_before_the_field() {
        #[derive(Serialize)]
        struct Before {
            #[serde(with = "serde_bytes")]
            scope_id: [u8; 32],
            #[serde(with = "serde_bytes")]
            path_hash: [u8; 32],
            #[serde(with = "serde_bytes")]
            sealed_path: Vec<u8>,
            change: GroupFileChange,
            #[serde(with = "serde_bytes")]
            generation_id: [u8; 32],
            stamped_at_ms: i64,
        }
        let now = core(None);
        let before = Before {
            scope_id: now.scope_id,
            path_hash: now.path_hash,
            sealed_path: now.sealed_path.clone(),
            change: now.change.clone(),
            generation_id: now.generation_id,
            stamped_at_ms: now.stamped_at_ms,
        };
        assert_eq!(
            canonical_encode(&now).unwrap(),
            canonical_encode(&before).unwrap()
        );
        let based = core(Some([0x44; 32]));
        let back: GroupFileCore = canonical_decode(&canonical_encode(&based).unwrap()).unwrap();
        assert_eq!(back, based, "the base round-trips");
    }

    /// Decision 10: the base rides inside the signed core, so a relay that
    /// strips or rewrites it breaks the author's signature.
    #[test]
    fn the_base_is_signed() {
        let device = SigningKey::from_bytes(&[0x21; 32]);
        let record = GroupFileRecord::sign(core(Some([0x44; 32])), &device, vec![9, 9]).unwrap();
        let verifies = |core: &GroupFileCore| {
            crate::identity::verify_detached(
                &device.verifying_key().to_bytes(),
                &group_file_signing_bytes(core, &record.authorization).unwrap(),
                &record.signature,
            )
        };
        assert!(verifies(&record.core));
        assert!(!verifies(&core(Some([0x45; 32]))), "a changed base");
        assert!(!verifies(&core(None)), "a stripped base");
    }

    /// A device cert carriage for `device`, signed by `signer`.
    fn carriage(signer: &crate::identity::ActorKeypair, device: [u8; 32]) -> Vec<u8> {
        let cert = crate::data::DeviceAuthorization {
            actor_id: signer.actor_id(),
            device_key: device,
            capabilities: vec![crate::data::Capability::RenewBearer],
            created_at: crate::data::Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = crate::encoding::sign_envelope(signer, &cert).unwrap();
        canonical_encode(&crate::encoding::EmbedAsBytes::from_signed(bytes, env)).unwrap()
    }

    /// Rule (i), arm (b′) (`group-set.md` decision 5, the 2026-11-02
    /// ruling): a removed member's row is honoured iff its signed stamp is
    /// not after the earliest honoured removal of its entry — a row stamped
    /// AT the removal instant stands, one stamped 1 ms after is refused,
    /// and a still-enrolled member's row is untouched, however late.
    #[test]
    fn a_removed_member_is_honoured_as_of_its_removal() {
        use crate::identity::ActorKeypair;
        let authority_actor = ActorKeypair::from_secret([21; 32]);
        let authority =
            GroupAuthority::build(&SCOPE, &authority_actor.actor_id(), &[], std::iter::empty());
        let member = |seed: u8| {
            let actor = ActorKeypair::from_secret([seed; 32]);
            let device = SigningKey::from_bytes(&[seed.wrapping_add(0x20); 32]);
            let core = RosterEntryCore {
                scope_id: SCOPE,
                member_actor: actor.actor_id(),
                admission_salt: [seed; 32],
            };
            let cert = carriage(&actor, device.verifying_key().to_bytes());
            (device, core, cert)
        };
        let (removed_device, removed_core, removed_cert) = member(0x31);
        let (kept_device, kept_core, kept_cert) = member(0x32);
        let removed_entry = roster_entry_id(&removed_core).unwrap();
        let kept_entry = roster_entry_id(&kept_core).unwrap();

        const REMOVED_AT: i64 = 1_700_000_500_000;
        // Two authority devices removed the member; the earlier instant binds.
        let removals: Vec<(String, Vec<u8>)> = [(0x61u8, REMOVED_AT + 9), (0x62, REMOVED_AT)]
            .into_iter()
            .map(|(seed, at)| {
                let device = SigningKey::from_bytes(&[seed; 32]);
                let (key, record) = crate::group_scope::sign_roster_removal(
                    &device,
                    removed_entry,
                    carriage(&authority_actor, device.verifying_key().to_bytes()),
                    at,
                );
                (key, canonical_encode(&record).unwrap())
            })
            .collect();
        let roster = RosterView::build(
            &SCOPE,
            &authority,
            removals.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
        );
        assert_eq!(roster.excluded_since(&removed_entry), Some(REMOVED_AT));
        assert_eq!(roster.excluded_since(&kept_entry), None);
        let mut enrolment = GroupGenerationEnrolment::default();
        enrolment
            .wrapped
            .insert(GEN, [removed_entry, kept_entry].into_iter().collect());

        let row = |device: &SigningKey, entry: &RosterEntryCore, cert: &[u8], at: i64| {
            let record = GroupFileRecord::sign(
                GroupFileCore {
                    stamped_at_ms: at,
                    writer_entry: Some(entry.clone()),
                    ..core(None)
                },
                device,
                cert.to_vec(),
            )
            .unwrap();
            let key = record.logical_key();
            (key, canonical_encode(&record).unwrap())
        };
        let verdict = |(key, value): (String, Vec<u8>)| {
            verify_group_file(&SCOPE, &key, &value, &authority, &roster, &enrolment)
        };

        verdict(row(
            &removed_device,
            &removed_core,
            &removed_cert,
            REMOVED_AT,
        ))
        .expect("a row stamped at the removal instant is a pre-removal write");
        let refused = verdict(row(
            &removed_device,
            &removed_core,
            &removed_cert,
            REMOVED_AT + 1,
        ))
        .expect_err("one stamped 1 ms after the removal is refused");
        assert!(refused.contains("stamped after the removal"), "{refused}");
        verdict(row(&kept_device, &kept_core, &kept_cert, i64::MAX / 2))
            .expect("an enrolled member's row is untouched by the bound");
    }

    #[test]
    fn a_sealed_path_opens_only_under_its_own_key_generation_and_cell() {
        let hash = group_file_path_hash(&SCOPE, "docs/plan.txt");
        let sealed = seal_group_file_path(&KEY, &GEN, &hash, "docs/plan.txt").unwrap();
        assert_eq!(
            sealed,
            seal_group_file_path(&KEY, &GEN, &hash, "docs/plan.txt").unwrap(),
            "convergent: a re-lift reseals byte-identically"
        );
        assert_eq!(
            open_group_file_path(&KEY, &SCOPE, &GEN, &hash, &sealed).unwrap(),
            "docs/plan.txt"
        );
        assert!(open_group_file_path(&[0x12; 32], &SCOPE, &GEN, &hash, &sealed).is_err());
        assert!(open_group_file_path(&KEY, &SCOPE, &[0x7b; 32], &hash, &sealed).is_err());
        let other = group_file_path_hash(&SCOPE, "docs/other.txt");
        assert!(open_group_file_path(&KEY, &SCOPE, &GEN, &other, &sealed).is_err());
        // A path sealed honestly under the wrong cell opens but is refused.
        let misfiled = seal_group_file_path(&KEY, &GEN, &other, "docs/plan.txt").unwrap();
        assert_eq!(
            open_group_file_path(&KEY, &SCOPE, &GEN, &other, &misfiled).unwrap_err(),
            "group file path does not hash to its cell"
        );
    }
}
