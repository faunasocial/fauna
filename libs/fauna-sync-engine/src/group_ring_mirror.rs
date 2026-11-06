//! The **group-ring mirror** — how a held group set's ring reaches a
//! capability host built with the app dead (`group-set.md` § Build design —
//! the group set, decision 12).
//!
//! A group set's keys are opened from the scope's machinery rows and this
//! member's group-reception keys ([`crate::group_set::held_group_generations`]),
//! and those rows never cross a nest: a capability host's throwaway fleet
//! replica (`on-demand-files-shared-sets.md` decision 1′) cannot rebuild them,
//! and the extension mounts no account store. So the **app's runtime mirrors
//! every held scope's ring** — the scope id, its folded name, its short id,
//! the tip and every retained generation's KEY — into one sealed file in the
//! account's sync state dir of the host's domain, the directory every
//! capability host of the account is built with (`state_dir`: android's
//! `syncStateDir()`, apple's container-domain root). The host opens it with
//! the [`GroupRingMirrorKey`] it derives from the `BackupKey` it was built
//! with, and derives the content keys itself
//! ([`MirroredGroupRing::content_keys`]) — one derivation, the host's.
//!
//! **Not the credential store.** The slot's retained-key carriage is one
//! item under `fauna_credential_store::MAX_ITEM_VALUE_BYTES` that trims on
//! overflow; a ring of N scopes × M generations is unbounded in both, and a
//! mirror that evicted the account's own retained keys to fit would cost the
//! seedless agent its only source. A file has no cap, and the mirror's write
//! touches no credential item — by construction, not by a guard.
//!
//! **One writer, whole-record.** Every write is the complete current ring
//! set, so a generation the plane shreds and a scope the device no longer
//! holds leave the mirror at the next write; a reader takes the file fresh
//! at every consult. The write is tmp+rename, so a reader never sees a torn
//! file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use fauna_core::aead_seal;
use fauna_core::crypto::{GenerationKey, GroupRingMirrorKey};
use fauna_core::encoding::{canonical_decode, canonical_encode};
use fauna_core::group_content::GroupContentKeys;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

/// The mirror's file name inside the account's sync state dir.
pub const MIRROR_FILE: &str = "group-rings.sealed";

/// File magic — the first bytes of every mirror file.
const MAGIC: &[u8; 15] = b"FAUNAGROUPRING\0";
/// The one format version this build writes and reads: the record schema
/// and this byte move together, so an older build refuses a newer file
/// loudly instead of mis-decoding a plaintext that authenticates.
const VERSION: u8 = 1;
/// The AEAD's context, versioned and NUL-terminated per the repo's
/// domain-separation convention; the actor id hex follows it in the AAD, so
/// a mirror cannot be moved between accounts.
const AAD_CONTEXT: &[u8] = b"fauna.group-ring-mirror.v1\0";

const HEADER_LEN: usize = MAGIC.len() + 1 + aead_seal::NONCE_LEN;

/// A 32-byte id as the mirror carries it — a byte string, never an integer
/// array.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Zeroize)]
pub struct Id32(#[serde(with = "serde_bytes")] pub [u8; 32]);

impl std::fmt::Debug for Id32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}…", hex::encode(&self.0[..4]))
    }
}

/// One retained generation of one scope: its id and its **generation key**
/// (never a content key — the host derives those).
#[derive(Clone, Serialize, Deserialize, Zeroize)]
pub struct MirroredGenerationKey {
    #[serde(with = "serde_bytes")]
    pub generation: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub key: [u8; 32],
}

impl std::fmt::Debug for MirroredGenerationKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key never reaches a log.
        f.debug_struct("MirroredGenerationKey")
            .field("generation", &Id32(self.generation))
            .finish_non_exhaustive()
    }
}

/// One held group set's ring as the mirror carries it — what the app-dead
/// host needs to list the set in its presence plan and to build its engine.
#[derive(Clone, Serialize, Deserialize, Zeroize)]
#[zeroize(drop)]
pub struct MirroredGroupRing {
    pub scope_id: Id32,
    /// The folded `fauna.group.name`; `None` for a nameless set.
    pub name: Option<String>,
    /// The short scope id — the nameless set's label.
    pub short_id: String,
    /// The admissible tip as the plane resolved it; `None` is the fail-safe
    /// gap (the host builds nothing and the row reads keys-pending).
    pub tip: Option<Id32>,
    /// Every retained generation this member holds a key for.
    pub keys: Vec<MirroredGenerationKey>,
}

impl std::fmt::Debug for MirroredGroupRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MirroredGroupRing")
            .field("scope_id", &self.scope_id)
            .field("name", &self.name)
            .field("short_id", &self.short_id)
            .field("tip", &self.tip)
            .field("generations", &self.keys.len())
            .finish()
    }
}

impl MirroredGroupRing {
    /// The ring as the host's engine takes it: every content key derived
    /// through `fauna_core::group_content::group_file_content_key` — the one
    /// derivation `group_set_keys` uses in the app — so the host can pick no
    /// other kind. A tip the ring holds no key for answers no tip.
    #[must_use]
    pub fn content_keys(&self) -> GroupContentKeys {
        let held: BTreeMap<[u8; 32], GenerationKey> = self
            .keys
            .iter()
            .map(|k| (k.generation, GenerationKey::from_bytes(k.key)))
            .collect();
        GroupContentKeys::from_generation_keys(self.tip.map(|t| t.0), held.iter())
    }

    /// The set's label: the folded name, else the short id (decision 7's
    /// label, as the folders page shows it).
    #[must_use]
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.short_id)
    }
}

/// The at-rest record — a named-field record so later fields join additively
/// under the same [`VERSION`] only when an older reader can ignore them. No
/// drop-zeroize of its own: its rings are moved out to the reader and each
/// ring zeroizes itself; the writer zeroizes the record it encoded by hand.
#[derive(Serialize, Deserialize, Zeroize)]
struct MirrorRecord {
    rings: Vec<MirroredGroupRing>,
}

/// The mirror file of one account, in one state dir.
#[derive(Clone, Debug)]
pub struct GroupRingMirror {
    path: PathBuf,
}

impl GroupRingMirror {
    /// The mirror inside `state_dir` — the account's sync state dir of the
    /// host's domain (`<base>/<actor-id-hex>/`), the `state_dir` every
    /// capability host of the account is built with.
    #[must_use]
    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(MIRROR_FILE),
        }
    }

    /// Where the mirror rests.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn aad(actor_id_hex: &str) -> Vec<u8> {
        let mut aad = AAD_CONTEXT.to_vec();
        aad.extend_from_slice(actor_id_hex.as_bytes());
        aad
    }

    /// Write `rings` as the whole mirror — sealed under `key`, bound to
    /// `actor_id_hex`, tmp+rename so a reader never sees a torn file, owner-only
    /// where the platform has modes.
    ///
    /// # Errors
    /// The record failing to encode, or the file failing to write.
    pub fn write(
        &self,
        key: &GroupRingMirrorKey,
        actor_id_hex: &str,
        rings: &[MirroredGroupRing],
    ) -> Result<()> {
        let mut record = MirrorRecord {
            rings: rings.to_vec(),
        };
        let encoded = canonical_encode(&record).context("encode the group-ring mirror");
        record.zeroize();
        let plain = Zeroizing::new(encoded?);
        let nonce = aead_seal::fresh_nonce();
        let sealed =
            aead_seal::seal_with_nonce(key.as_bytes(), &nonce, &Self::aad(actor_id_hex), &plain);
        let mut bytes = Vec::with_capacity(HEADER_LEN + sealed.len());
        bytes.extend_from_slice(MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&sealed);

        let dir = self
            .path
            .parent()
            .context("the mirror path has no parent directory")?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create the mirror's directory {}", dir.display()))?;
        let tmp = self.path.with_extension("sealed.tmp");
        {
            let mut open = std::fs::OpenOptions::new();
            open.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                open.mode(0o600);
            }
            let mut file = open
                .open(&tmp)
                .with_context(|| format!("open {} for the mirror", tmp.display()))?;
            use std::io::Write as _;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .with_context(|| format!("write the mirror to {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("move the mirror into place at {}", self.path.display()))?;
        Ok(())
    }

    /// Read the mirror, fresh. **A missing file is an empty mirror** — the
    /// device holds no group set the app has mirrored yet; a file that is
    /// present but malformed, from another version, sealed for another
    /// account or under another key is an **error**, never an empty answer:
    /// a presence door tearing down every group presence on an unreadable
    /// mirror is the failure this distinction prevents.
    ///
    /// # Errors
    /// The file failing to read or to open.
    pub fn read(
        &self,
        key: &GroupRingMirrorKey,
        actor_id_hex: &str,
    ) -> Result<Vec<MirroredGroupRing>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("read the mirror at {}", self.path.display()));
            }
        };
        anyhow::ensure!(
            bytes.len() >= HEADER_LEN && &bytes[..MAGIC.len()] == MAGIC,
            "the mirror at {} is not a group-ring mirror",
            self.path.display()
        );
        let version = bytes[MAGIC.len()];
        anyhow::ensure!(
            version == VERSION,
            "the mirror at {} is format version {version}; this build reads {VERSION}",
            self.path.display()
        );
        let nonce: [u8; aead_seal::NONCE_LEN] = bytes[MAGIC.len() + 1..HEADER_LEN]
            .try_into()
            .expect("the header length covers the nonce");
        let plain = aead_seal::open_with_nonce(
            key.as_bytes(),
            &nonce,
            &Self::aad(actor_id_hex),
            &bytes[HEADER_LEN..],
        )
        .with_context(|| {
            format!(
                "the mirror at {} did not open — a wrong key, another account's mirror or a tampered file",
                self.path.display()
            )
        })?;
        let MirrorRecord { rings } =
            canonical_decode(&plain).context("decode the group-ring mirror")?;
        Ok(rings)
    }
}

/// A content digest of `rings` — what a writer compares to skip a write
/// that would change nothing.
#[must_use]
pub fn digest(rings: &[MirroredGroupRing]) -> [u8; 32] {
    let mut record = MirrorRecord {
        rings: rings.to_vec(),
    };
    let plain = Zeroizing::new(canonical_encode(&record).unwrap_or_default());
    record.zeroize();
    *blake3::hash(&plain).as_bytes()
}

/// Write `rings` unless `last` already names their digest; answers whether
/// a write happened and updates `last` on success.
///
/// # Errors
/// [`GroupRingMirror::write`]'s.
pub fn write_if_changed(
    mirror: &GroupRingMirror,
    key: &GroupRingMirrorKey,
    actor_id_hex: &str,
    rings: &[MirroredGroupRing],
    last: &mut Option<[u8; 32]>,
) -> Result<bool> {
    let now = digest(rings);
    if *last == Some(now) {
        return Ok(false);
    }
    mirror.write(key, actor_id_hex, rings)?;
    *last = Some(now);
    Ok(true)
}

/// Every held group set's ring, read from the runtime: the sets
/// [`crate::offline_share::held_group_scope_views`] lists (the strict reader
/// both presence doors share), each ring opened by
/// [`crate::group_set::held_group_generations`] from the scope's rows and
/// this account's group-reception keys. **Strict**: a scope that will not
/// read is an error, never a shorter mirror.
///
/// # Errors
/// The ceremony record, a scope's rows or the reception keys failing to read.
#[cfg(all(feature = "p2p-share", feature = "account-runtime"))]
pub async fn held_rings(
    account: &crate::account_runtime::AccountStoreHandle,
    own: &fauna_core::identity::ActorId,
) -> Result<Vec<MirroredGroupRing>> {
    let views = crate::offline_share::held_group_scope_views(account, own).await?;
    let reception = account
        .group_reception_keys()
        .await
        .context("the group-reception keys")?;
    let mut rings = Vec::with_capacity(views.len());
    for view in views {
        let rows = account
            .group_scope_states(view.scope_id)
            .await
            .with_context(|| format!("group scope {} rows", view.short_id))?;
        let held = crate::group_set::held_group_generations(&view.scope_id, &rows, &reception);
        rings.push(MirroredGroupRing {
            scope_id: Id32(view.scope_id),
            name: view.name,
            short_id: view.short_id,
            tip: held.tip_id().map(Id32),
            keys: held
                .held()
                .map(|(generation, key)| MirroredGenerationKey {
                    generation: *generation,
                    key: *key.as_bytes(),
                })
                .collect(),
        });
    }
    Ok(rings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> GroupRingMirrorKey {
        GroupRingMirrorKey::derive(&fauna_core::crypto::BackupKey::from_bytes([b; 32]))
    }

    fn ring(scope: u8, name: Option<&str>, generations: u8, tip: bool) -> MirroredGroupRing {
        let keys: Vec<_> = (0..generations)
            .map(|g| MirroredGenerationKey {
                generation: [g; 32],
                key: [g ^ 0x5A; 32],
            })
            .collect();
        MirroredGroupRing {
            scope_id: Id32([scope; 32]),
            name: name.map(str::to_string),
            short_id: format!("{scope:02x}{scope:02x}{scope:02x}"),
            tip: (tip && generations > 0).then(|| Id32([generations - 1; 32])),
            keys,
        }
    }

    fn same(a: &MirroredGroupRing, b: &MirroredGroupRing) -> bool {
        a.scope_id == b.scope_id
            && a.name == b.name
            && a.short_id == b.short_id
            && a.tip == b.tip
            && a.keys.len() == b.keys.len()
            && a.keys
                .iter()
                .zip(&b.keys)
                .all(|(x, y)| x.generation == y.generation && x.key == y.key)
    }

    /// A ring round-trips whole — a named set, a nameless one, one with no
    /// tip — and the host's content keys are the app's derivation.
    #[test]
    fn a_ring_round_trips_and_derives_the_content_keys_the_app_would() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = GroupRingMirror::in_state_dir(tmp.path());
        let rings = vec![
            ring(1, Some("holiday"), 3, true),
            ring(2, None, 1, true),
            ring(3, Some("keys pending"), 2, false),
        ];
        mirror.write(&key(1), "aa", &rings).unwrap();
        let back = mirror.read(&key(1), "aa").unwrap();
        assert_eq!(back.len(), rings.len());
        for (a, b) in rings.iter().zip(&back) {
            assert!(same(a, b), "{a:?} vs {b:?}");
        }
        assert_eq!(back[0].label(), "holiday");
        assert_eq!(back[1].label(), "020202");

        let held: BTreeMap<[u8; 32], GenerationKey> = rings[0]
            .keys
            .iter()
            .map(|k| (k.generation, GenerationKey::from_bytes(k.key)))
            .collect();
        let app_side = GroupContentKeys::from_generation_keys(Some([2; 32]), held.iter());
        let host_side = back[0].content_keys();
        assert_eq!(host_side.tip().map(|(id, _)| id), Some([2; 32]));
        assert_eq!(
            host_side.tip().map(|(_, k)| *k),
            app_side.tip().map(|(_, k)| *k)
        );
        assert_eq!(host_side.key(&[0; 32]), app_side.key(&[0; 32]));
        assert!(
            back[2].content_keys().tip().is_none(),
            "no tip stays no tip"
        );
    }

    /// The carriage is a file, so the measurement that refuted the slot
    /// carriage (about eighteen entries for the whole account) does not
    /// apply: sixty-four scopes of thirty-two generations each round-trip
    /// whole, in one write far over the credential item cap, and nothing is
    /// evicted — the mirror never touches a credential item at all.
    #[test]
    fn the_whole_ring_set_fits_far_past_the_credential_item_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = GroupRingMirror::in_state_dir(tmp.path());
        let rings: Vec<_> = (0..64u8)
            .map(|s| ring(s, Some("a set with a name of some length"), 32, true))
            .collect();
        mirror.write(&key(1), "aa", &rings).unwrap();
        let on_disk = std::fs::metadata(mirror.path()).unwrap().len() as usize;
        assert!(
            on_disk > 4 * fauna_credential_store::MAX_ITEM_VALUE_BYTES,
            "{on_disk} bytes on disk — the carriage must not be a credential-item-sized thing"
        );
        let back = mirror.read(&key(1), "aa").unwrap();
        assert_eq!(back.len(), 64);
        assert!(back.iter().all(|r| r.keys.len() == 32), "nothing evicted");
    }

    /// Missing is empty; present-but-unopenable is an error — a wrong key,
    /// another account's mirror, a truncated or foreign file, a later
    /// version.
    #[test]
    fn missing_is_empty_but_unreadable_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = GroupRingMirror::in_state_dir(tmp.path());
        assert!(mirror.read(&key(1), "aa").unwrap().is_empty());

        mirror
            .write(&key(1), "aa", &[ring(1, None, 1, true)])
            .unwrap();
        assert!(mirror.read(&key(2), "aa").is_err(), "a wrong key");
        assert!(mirror.read(&key(1), "bb").is_err(), "another account");

        let bytes = std::fs::read(mirror.path()).unwrap();
        std::fs::write(mirror.path(), &bytes[..HEADER_LEN + 3]).unwrap();
        assert!(mirror.read(&key(1), "aa").is_err(), "truncated");
        std::fs::write(mirror.path(), b"not a mirror at all").unwrap();
        assert!(mirror.read(&key(1), "aa").is_err(), "foreign bytes");
        let mut later = bytes.clone();
        later[MAGIC.len()] = VERSION + 1;
        std::fs::write(mirror.path(), &later).unwrap();
        assert!(mirror.read(&key(1), "aa").is_err(), "a later version");
    }

    /// A pass writes only when the rings changed; a whole-record write
    /// drops what the plane no longer holds.
    #[test]
    fn a_pass_writes_on_change_only_and_a_write_is_the_whole_record() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = GroupRingMirror::in_state_dir(tmp.path());
        let mut last = None;
        let two = vec![ring(1, None, 2, true), ring(2, None, 1, true)];
        assert!(write_if_changed(&mirror, &key(1), "aa", &two, &mut last).unwrap());
        assert!(!write_if_changed(&mirror, &key(1), "aa", &two, &mut last).unwrap());
        let one = vec![ring(1, None, 1, true)];
        assert!(write_if_changed(&mirror, &key(1), "aa", &one, &mut last).unwrap());
        let back = mirror.read(&key(1), "aa").unwrap();
        assert_eq!(
            back.len(),
            1,
            "the shredded generation and the dropped scope left"
        );
        assert_eq!(back[0].keys.len(), 1);
        assert!(
            !mirror.path().with_extension("sealed.tmp").exists(),
            "no tmp left"
        );
    }
}
