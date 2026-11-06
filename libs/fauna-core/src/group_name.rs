//! **A group set's display name** — the `fauna.group.name` value and the
//! writer check every reader of it runs (`p2p-shared-set-build.md` § Build
//! design — the group set, decision 2: "one cell, the set's display name,
//! written by the authority").
//!
//! The name is one `LatestWins` cell, sealed under the generation tip, so
//! every member can seal a value into it. The entry envelope's writer
//! signature names whichever replica last re-published the cell, so the
//! authorship rides inside the value, in the shape of
//! [`crate::group_file::GroupFileRecord`]: a signed core, the author
//! device's `DeviceAuthorization` carriage, and a signature over both. A
//! reader honours a name only when the carriage passes the authority line's
//! revocation-aware [`GroupAuthority::verify_device_cert`], so a member's
//! rename is refused, and a name written by an authority device that has
//! since been revoked is retired with that device's other signatures.

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::encoding::{canonical_decode, canonical_encode};
use crate::error::Error;
use crate::group_scope::GroupAuthority;

/// Domain-separation tag for a group name's author signature. **Frozen**:
/// every name an authority device ever wrote verifies through it.
pub const GROUP_NAME_SIG_CONTEXT: &[u8] = b"fauna.group.name.sig.v1 2026-10-29\0";

/// Everything a name asserts — exactly what its author signs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupNameCore {
    /// The group scope the name belongs to. Signed, so a name is never
    /// replayed into another scope the same authority runs.
    #[serde(with = "serde_bytes")]
    pub scope_id: [u8; 32],
    /// The display name, UTF-8.
    pub name: String,
    /// The author's clock, unix ms — the `LwwStamp` the entry merges under
    /// carries exactly this instant and the author device, so a re-publisher
    /// cannot re-rank a signed name.
    pub stamped_at_ms: i64,
}

/// One `fauna.group.name` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupNameRecord {
    /// The signed assertion.
    pub core: GroupNameCore,
    /// The author device's `DeviceAuthorization`, in the `EmbedAsBytes`
    /// carriage every witness surface uses.
    #[serde(with = "serde_bytes")]
    pub authorization: Vec<u8>,
    /// Ed25519 by the authorized device over [`group_name_signing_bytes`].
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

/// The exact bytes a name's author signs: the frozen tag, the BLAKE3 of the
/// canonical core, and the BLAKE3 of the carried authorization (signed, so a
/// copy with the cert swapped no longer verifies — the file row's shape).
///
/// # Errors
/// Only on a core that fails canonical encoding.
pub fn group_name_signing_bytes(
    core: &GroupNameCore,
    authorization: &[u8],
) -> Result<Vec<u8>, Error> {
    let core_bytes = canonical_encode(core)?;
    let mut out = Vec::with_capacity(GROUP_NAME_SIG_CONTEXT.len() + 64);
    out.extend_from_slice(GROUP_NAME_SIG_CONTEXT);
    out.extend_from_slice(blake3::hash(&core_bytes).as_bytes());
    out.extend_from_slice(blake3::hash(authorization).as_bytes());
    Ok(out)
}

impl GroupNameRecord {
    /// Sign `core` as `device`, carrying `authorization` (its cert).
    ///
    /// # Errors
    /// Only on a core that fails canonical encoding.
    pub fn sign(
        core: GroupNameCore,
        device: &SigningKey,
        authorization: Vec<u8>,
    ) -> Result<GroupNameRecord, Error> {
        let signature = device
            .sign(&group_name_signing_bytes(&core, &authorization)?)
            .to_bytes()
            .to_vec();
        Ok(GroupNameRecord {
            core,
            authorization,
            signature,
        })
    }
}

/// A name a reader honours, with the device its checks established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HonouredGroupName {
    /// The verified record.
    pub record: GroupNameRecord,
    /// The authority device that signed it — the `LwwStamp` writer the entry
    /// must merge under.
    pub device: [u8; 32],
}

impl HonouredGroupName {
    /// The display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.record.core.name
    }
}

/// **The name's writer check**: decode `value` for scope `scope_id` and
/// honour it iff its carriage is a current authority device's — chained to
/// the authority root or a prior identity, unexpired at the signed instant,
/// not revoked on `authority`'s line — and the signature verifies under that
/// device. A member's device chains to the member, never to the authority,
/// so a member-written name fails the first arm.
///
/// # Errors
/// The refusal reason — a name failing any arm is counted `refused` and
/// never folded.
pub fn verify_group_name(
    scope_id: &[u8; 32],
    value: &[u8],
    authority: &GroupAuthority,
) -> Result<HonouredGroupName, String> {
    let record: GroupNameRecord =
        canonical_decode(value).map_err(|e| format!("set name does not decode: {e}"))?;
    let core = &record.core;
    if core.scope_id != *scope_id {
        return Err("set name names another scope".into());
    }
    let device = authority.verify_device_cert(
        "set name authorization",
        &record.authorization,
        core.stamped_at_ms,
    )?;
    let signed =
        group_name_signing_bytes(core, &record.authorization).map_err(|e| e.to_string())?;
    if !crate::identity::verify_detached(&device, &signed, &record.signature) {
        return Err("set name signature does not verify".into());
    }
    Ok(HonouredGroupName { device, record })
}
