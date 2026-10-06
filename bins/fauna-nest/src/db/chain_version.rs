//! Which preimage a tamper-evidence chain row was hashed under — recorded on
//! the row, never guessed — and the one recomputation each recorded version
//! selects.
//!
//! Two chains live under this rule: `audit_log.entry_hash` (written by
//! [`crate::db::admin::audit_on_conn`]) and `pending_actions.chain_hash`
//! (written by `CacheDb::create_pending_action`). Both hash a length-framed,
//! domain-separated preimage under a versioned domain tag, and record the
//! version beside the hash (`entry_hash_version` / `chain_hash_version`).
//!
//! # Why the version has to be data on the row
//!
//! A versioned domain tag makes two preimages **non-colliding**: no row hashed
//! under one format can be read as another. It does not make them
//! non-**interchangeable**. With nothing recording which format a row was
//! written under, a verifier certifying a chain that straddles a format change
//! has to keep every older arm live for *every* row it ever sees, forever:
//!
//!   - it can never be retired, because no query can name the last old row;
//!   - it can never be scoped, because no row says it is entitled to the
//!     newer format;
//!   - and so a certified row tells an admin nothing about **which** arm
//!     certified it. The verification's meaning collapses to its weakest arm
//!     across the whole log.
//!
//! The recorded version is what bounds that: a row is recomputed under the
//! format it declares and *nothing else*. Today one format exists — `2`, the
//! length-framed preimage. The numbers `0` (the pre-record "unrecorded"
//! verdict) and `1` (the unframed concatenation, which bound the concatenation
//! of its columns and not their boundaries) retired with the nest schema's
//! genesis (`version-compatibility.md` § Dimension 2, program 4): every writer
//! has stamped `2` since the record landed and no older row rests anywhere,
//! so both now read as versions this binary does not know. A future format
//! change adds a variant and a match arm here, and bumps [`ChainVersion::CURRENT`].
//!
//! # The refuse rule
//!
//! A version this binary does not know yields `None` from
//! [`ChainVersion::from_recorded`]. Callers must treat that as *unverifiable*
//! — never as an invitation to try another format until one matches. Trying
//! formats is the interchangeability this module exists to remove, and it is
//! the shape a well-meaning implementer reaches for first.

use crate::domain_hash::{HashField, write_fields};

/// A recorded preimage version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainVersion {
    /// `2` — length-framed under a versioned domain tag.
    V2,
}

impl ChainVersion {
    /// What every new row is written under.
    pub(crate) const CURRENT: ChainVersion = ChainVersion::V2;

    /// The stored column value.
    pub(crate) const fn as_i64(self) -> i64 {
        match self {
            ChainVersion::V2 => 2,
        }
    }

    /// The recorded value, or `None` for a version this binary does not know —
    /// the refuse rule. A future v3 read by this binary lands here, and so do
    /// the retired `0` and `1`; the correct answer is "I cannot verify this",
    /// never "let me try v2".
    pub(crate) const fn from_recorded(v: i64) -> Option<ChainVersion> {
        match v {
            2 => Some(ChainVersion::V2),
            _ => None,
        }
    }
}

/// Domain tag for the v2 `audit_log` entry hash.
pub(crate) const AUDIT_ENTRY_DST_V2: &[u8] = b"fauna.audit_log.entry.v2";

/// Domain tag for the v2 `pending_actions` chain hash.
pub(crate) const PENDING_ACTION_DST_V2: &[u8] = b"fauna.pending_actions.chain.v2";

/// The columns an `audit_log` row's `entry_hash` covers. Borrowed rather than
/// owned so the writer can hash straight from its own parameters and a
/// verifier straight from a served row.
pub(crate) struct AuditPreimage<'a> {
    pub id: i64,
    pub ts: i64,
    pub actor_id: Option<&'a [u8]>,
    pub action: &'a str,
    pub target: Option<&'a str>,
    pub detail: Option<&'a str>,
    pub prev_hash: &'a str,
}

/// Recompute an `audit_log` row's `entry_hash` under the version the row
/// **records**. A verifier gets the version from
/// [`ChainVersion::from_recorded`], whose `None` is the refuse rule; there is
/// deliberately no fallback arm — see the module docs.
///
/// This is the same function [`crate::db::admin::audit_on_conn`] writes with,
/// so a writer and a verifier cannot drift apart field-for-field.
pub(crate) fn audit_entry_hash(version: ChainVersion, p: &AuditPreimage<'_>) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    match version {
        ChainVersion::V2 => {
            write_fields(
                AUDIT_ENTRY_DST_V2,
                &[
                    HashField::I64(p.id),
                    HashField::I64(p.ts),
                    // `None` and `Some(&[])` both frame as length 0; a
                    // production actor_id is either absent or a 32-byte id,
                    // never empty.
                    HashField::LenPrefixed(p.actor_id.unwrap_or(&[])),
                    HashField::LenPrefixed(p.action.as_bytes()),
                    HashField::OptStr(p.target),
                    HashField::OptStr(p.detail),
                    // Trailing, and a fixed-width hex digest in every
                    // non-genesis row.
                    HashField::Trailing(p.prev_hash.as_bytes()),
                ],
                |b| hasher.update(b),
            );
        }
    }
    format!("{:x}", hasher.finalize())
}

/// The status every `pending_actions` chain hash is computed over.
///
/// The hash is computed once at insert, and the later
/// `executed`/`expired`/`cancelled` transitions deliberately do not recompute
/// it — so a stored preimage's status is always the creation-time constant. A
/// verifier must hash this, never the row's current `status` column.
pub(crate) const PENDING_ACTION_CREATION_STATUS: &str = "pending";

/// The columns a `pending_actions` row's `chain_hash` covers. `status` is the
/// creation-time value — see [`PENDING_ACTION_CREATION_STATUS`].
pub(crate) struct PendingActionPreimage<'a> {
    pub prev_hash: &'a [u8],
    pub action_type: &'a str,
    pub actor_id: &'a [u8],
    pub created_at: i64,
    pub status: &'a str,
}

/// Recompute a `pending_actions` row's `chain_hash` under the version the row
/// records, on the same terms as [`audit_entry_hash`].
pub(crate) fn pending_action_chain_hash(
    version: ChainVersion,
    p: &PendingActionPreimage<'_>,
) -> Vec<u8> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    match version {
        ChainVersion::V2 => {
            write_fields(
                PENDING_ACTION_DST_V2,
                &[
                    HashField::LenPrefixed(p.prev_hash),
                    HashField::LenPrefixed(p.action_type.as_bytes()),
                    HashField::LenPrefixed(p.actor_id),
                    HashField::I64(p.created_at),
                    HashField::Trailing(p.status.as_bytes()),
                ],
                |b| hasher.update(b),
            );
        }
    }
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>(action: &'a str, target: &'a str) -> AuditPreimage<'a> {
        AuditPreimage {
            id: 7,
            ts: 1_700_000_000,
            actor_id: Some(&[1u8; 32]),
            action,
            target: Some(target),
            detail: None,
            prev_hash: "00ff",
        }
    }

    /// The refuse rule on the version axis: a number this binary does not know
    /// is not verifiable, and no arm tries anything else. The retired `0`
    /// (unrecorded) and `1` (the unframed preimage) are unknown numbers now.
    #[test]
    fn an_unknown_version_is_refused_rather_than_guessed() {
        for v in [0, 1, 3, -1, i64::MAX] {
            assert_eq!(ChainVersion::from_recorded(v), None, "version {v}");
        }
    }

    /// The framing binds the column boundaries, not just their concatenation:
    /// a re-partition recomputes to a different digest, on both chains.
    #[test]
    fn a_re_partition_changes_the_digest_on_both_chains() {
        assert_ne!(
            audit_entry_hash(
                ChainVersion::V2,
                &sample("role.grant.superadmin", "mallory")
            ),
            audit_entry_hash(
                ChainVersion::V2,
                &sample("role.grant", ".superadminmallory")
            ),
        );
        let preimage = |action_type, actor_id| PendingActionPreimage {
            prev_hash: b"prev",
            action_type,
            actor_id,
            created_at: 9,
            status: PENDING_ACTION_CREATION_STATUS,
        };
        assert_ne!(
            pending_action_chain_hash(ChainVersion::V2, &preimage("account", b".delete")),
            pending_action_chain_hash(ChainVersion::V2, &preimage("account.delete", b"")),
        );
    }

    /// The stored column value round-trips through the refuse gate, so a
    /// writer stamping `as_i64` and a verifier reading `from_recorded` cannot
    /// disagree about what a number means.
    #[test]
    fn the_stored_value_round_trips() {
        let v = ChainVersion::CURRENT;
        assert_eq!(ChainVersion::from_recorded(v.as_i64()), Some(v));
    }
}
