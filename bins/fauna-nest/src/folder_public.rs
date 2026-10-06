//! The **public folder read plane** — the one core both the client kind
//! `fauna.folders.public.fetch` and its federation twin
//! `fauna.federation.folder.public.fetch` authorize through.
//!
//! Owner docs: `docs/goal/behavior/folders.md` § Publicly-synced follow
//! (address / floor / strip / flip-back semantics) and
//! `docs/goal/architecture/federation.md` § The public folder read plane
//! (kinds, gates, wire rules). Phase 4 slice 4f-i of the folders re-model.
//!
//! **Why this is its own module, authorizing independently.** Every other
//! folder read resolves through [`crate::folder_authz::can_read_folder`], whose
//! gate is *membership*. The ratified design refuses to grow a world-readability
//! arm inside that resolver: an OR-ed public arm would flow into
//! `authorize_snapshot`, the folder listing, and every future caller of "may
//! this actor read?", where one bug opens **member** data. So the public plane
//! gets the inverse-shaped gate instead — **serve iff the addressed row's
//! current `audience == 'public'`, else `not_found`** — and nothing but a public
//! folder is reachable through this module by construction.
//!
//! **Everything folds to `not_found`.** Absent, private, group-bound-but-not-
//! declassified, reserved rail: one indistinguishable answer (ST-RES-1). Public
//! folder *names* are world-readable by the ratified exception, so name-addressed
//! resolution of a public folder is not an oracle; every other existence
//! question stays folded.
//!
//! **This module writes nothing.** A follow leaves no row on the home nest — no
//! roster, no registration, no per-follower state — so followers are not
//! enumerable and a follower flood cannot grow nest state.

use fauna_protocol::RpcError;
use fauna_protocol::sync::SyncChange;

use crate::AppState;
use crate::folder_authz::FolderReadGrant;
use crate::rpc_errors::not_found_ns;

/// Error namespace for the public read plane, shared by both kinds.
const NS: &str = "folders";

/// How a caller addresses a public folder (`folders.md` § Publicly-synced
/// follow, the address bullet).
///
/// The follow gesture starts from a handle + the folder's plaintext name; the
/// first successful read pins the home nest's stable [`Self::Id`], which every
/// later read uses — so a rename never breaks an established follow.
#[derive(Debug, Clone)]
pub(crate) enum PublicFolderAddress {
    /// `(owner_actor_id, plaintext folder name)` — the first-contact form.
    OwnerAndName([u8; 32], String),
    /// The home nest's stable `folders.id`, pinned by a prior successful read.
    Id(i64),
}

/// The one refusal this plane ever produces. Kept private so no caller can
/// accidentally distinguish "absent" from "not declassified" by picking a
/// different error.
fn refuse() -> RpcError {
    not_found_ns(NS, "no public folder at that address")
}

/// Resolve + gate an address, returning the folder row **iff it is currently
/// public**.
///
/// The audience is read from the folder's *current* row on every request, which
/// is what makes a flip-back an immediate revoke: the next read after
/// `public → private`/`shared` answers [`refuse`], indistinguishable from
/// absent — that *is* the revoke semantics.
///
/// Returns the row **and the grant it was admitted under**, the same shape
/// [`crate::folder_authz::can_read_folder`] answers in — so a public read
/// projects labels through the same `is_label_audience` predicate every other
/// folder read surface uses, rather than open-coding "the world gets no
/// labels" here.
pub(crate) async fn resolve_public_folder(
    state: &AppState,
    address: &PublicFolderAddress,
) -> Result<(crate::db::FolderRow, FolderReadGrant), RpcError> {
    let row = match address {
        PublicFolderAddress::OwnerAndName(owner, name) => state
            .db
            .get_folder_for_actor(name, owner)
            .await
            .map_err(|e| crate::rpc_errors::internal_ns(NS, e))?,
        PublicFolderAddress::Id(id) => state
            .db
            .get_folder_by_id(*id)
            .await
            .map_err(|e| crate::rpc_errors::internal_ns(NS, e))?,
    };
    let row = row.ok_or_else(refuse)?;

    // THE GATE. Read straight off the column rather than through
    // `folder_handlers::audience_of`, because only the explicit
    // declassification admits here — `audience_of`'s derived `"shared"` arm
    // (bound but never declassified) must NOT, and asking the derivation would
    // invite a future edit to widen it.
    if row.audience.as_deref() != Some("public") {
        return Err(refuse());
    }
    // Belt-and-braces, fail-closed: a reserved `__` rail is infrastructure and
    // can never be declassified (`folder_handlers`' transition validator
    // refuses it), so this is unreachable today. It costs one comparison to
    // keep it unreachable if that validator ever loosens.
    if crate::db::snapshots::is_reserved_folder_name(&row.name) {
        return Err(refuse());
    }
    Ok((row, FolderReadGrant::Public))
}

/// Strip a change row to the **public projection** (`folders.md` § Publicly-
/// synced follow, the stripped-projection bullet).
///
/// A follower needs paths, manifest refs, sizes, types, timestamps and the
/// causal watermark. They must never receive the owner's device fleet or
/// authorship map — nor key/label metadata that is meaningless without a key
/// they will never hold:
///
/// - `device_id` — the owner's device fleet.
/// - `author_actor_id` — who wrote what, the authorship map.
/// - `path_sealed` — the sealed label; a public folder's paths ride plaintext
///   in `path`, so this only ever carries residue from before the flip, and
///   shipping it would hand out a salt over a user-chosen string (the finding
///   behind [`crate::folder_authz::FolderReadGrant::is_label_audience`]).
/// - `content_key_version` — the M2 generation. There is no group and no key on
///   this plane; the field could only mislead.
/// - `signature` / `signer_key` — the writer signature (`mls-group-key-material.md`
///   § M2 → *Writer-signed change records*). The key names the writer's
///   device principal (the device fleet again), and the statement it signs
///   covers `device_id` and the author, both stripped above, so a follower
///   could never rebuild it: the pair would be pure disclosure.
///
/// **How "absent" lands on the wire.** `author_actor_id`, `path_sealed` and
/// `content_key_version` carry `skip_serializing_if`, so a stripped row omits
/// their keys outright. `device_id` does **not** — it is one of `SyncChange`'s
/// original fields, with neither `skip_serializing_if` nor `#[serde(default)]`
/// — so it rides as an explicit `null`. That asymmetry is deliberate and must
/// stay: adding `skip_serializing_if` would make a new nest emit a row whose
/// missing `device_id` fails to decode on every peer and client that lacks the
/// matching `#[serde(default)]`, a wire-compat break inside a major
/// (`version-compatibility.md`). A `null` conveys nothing about the owner's
/// device fleet, so the confidentiality property is identical; only the
/// encoding differs. The contract this function owes is therefore **no value**,
/// not **no key**.
pub(crate) fn strip_for_public(mut change: SyncChange, grant: FolderReadGrant) -> SyncChange {
    change.device_id = None;
    change.author_actor_id = None;
    change.content_key_version = None;
    change.signature = None;
    change.signer_key = None;
    // The label projection follows the GRANT, uniformly with every other folder
    // read surface, rather than open-coding the rule here — the one predicate
    // means a future grant class cannot be given labels at one surface and
    // denied them at another. `Public` is not label audience (a public folder's
    // paths ride plaintext in `path`; a lingering seal from before the flip
    // would only hand out its salt), so the envelope is withheld.
    if !grant.is_label_audience() {
        change.path_sealed = None;
    }
    change
}

/// One page of a public folder's change log: rows above **both** the caller's
/// `since` cursor and the folder's public floor, stripped, frame-budgeted.
///
/// The floor is the structural half of the boundary (`folders.md` § Publicly-
/// synced follow): nothing recorded before the most recent flip-to-public is
/// ever served here, so the private era's metadata — edit timing, sizes, counts,
/// sealed labels — never crosses this plane, without any per-row classification
/// to get wrong. `max(since, floor)` is that boundary and the cursor in one
/// comparison, applied *at the query*, so a floored row is never even loaded.
pub(crate) async fn public_changes_page(
    state: &AppState,
    folder: &crate::db::FolderRow,
    grant: FolderReadGrant,
    since: i64,
    limit: i64,
) -> Result<Vec<SyncChange>, RpcError> {
    let floor = since.max(folder.public_floor_seq);
    let limit = crate::segments::effective_fetch_limit(limit);
    let mut rows = state
        .db
        .get_sync_changes_for_folder(folder.id, floor, None)
        .await
        .map_err(|e| crate::rpc_errors::internal_ns(NS, e))?;
    rows.truncate(limit as usize);
    // Frame-budget exactly like the member `folder.changes.fetch` page — close
    // early, never skip, so the follower's cursor stays contiguous.
    let (rows, rest) = crate::segments::take_page_within_budget(rows, |c| {
        c.wire_len() + crate::segments::RECORD_WIRE_OVERHEAD
    });
    if rows.is_empty()
        && let Some(head) = rest.first()
    {
        tracing::error!(
            folder_id = folder.id,
            seq = head.seq,
            "public folder fetch: a single change row exceeds the WS frame \
             budget — a follower's read cannot advance"
        );
    }
    Ok(rows
        .iter()
        .map(crate::sync_handlers::change_to_wire)
        .map(|c| strip_for_public(c, grant))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_change_with_everything() -> SyncChange {
        SyncChange {
            seq: 7,
            path_hash: "aa".repeat(32),
            manifest_hash: Some("bb".repeat(32)),
            size_bytes: 12,
            change_type: "create".to_string(),
            created_at: 1,
            path: Some("notes/a.md".to_string()),
            device_id: Some("cc".repeat(32)),
            content_key_version: Some(4),
            thumbnail_hash: Some("dd".repeat(32)),
            author_actor_id: Some("ee".repeat(32)),
            path_sealed: Some(fauna_protocol::ByteBuf::from(vec![1, 2, 3])),
            derived_through: Some(5),
            is_resolution: Some(false),
            is_retention: Some(false),
            item_class: None,
            origin_writer: None,
            origin_seq: None,
            entry: None,
            extra: Default::default(),
            signature: Some(fauna_protocol::ByteBuf::from(vec![0x5a; 64])),
            signer_key: Some(fauna_protocol::ByteBuf::from(vec![0xa5; 32])),
        }
    }

    /// The strip rule, field by field: the identity/key fields go, and
    /// everything a follower legitimately needs survives untouched.
    #[test]
    fn strip_removes_exactly_the_four_identity_fields() {
        let stripped = strip_for_public(wire_change_with_everything(), FolderReadGrant::Public);

        assert_eq!(stripped.device_id, None, "the owner's device fleet");
        assert_eq!(stripped.author_actor_id, None, "the authorship map");
        assert_eq!(stripped.path_sealed, None, "the sealed label + its salt");
        assert_eq!(stripped.content_key_version, None, "the M2 generation");
        assert_eq!(stripped.signature, None, "the writer signature");
        assert_eq!(stripped.signer_key, None, "the writer's device key");

        let kept = wire_change_with_everything();
        assert_eq!(stripped.seq, kept.seq);
        assert_eq!(stripped.path_hash, kept.path_hash);
        assert_eq!(stripped.manifest_hash, kept.manifest_hash);
        assert_eq!(stripped.size_bytes, kept.size_bytes);
        assert_eq!(stripped.change_type, kept.change_type);
        assert_eq!(stripped.created_at, kept.created_at);
        assert_eq!(stripped.path, kept.path, "paths are the point of a follow");
        assert_eq!(stripped.thumbnail_hash, kept.thumbnail_hash);
        assert_eq!(
            stripped.derived_through, kept.derived_through,
            "the causal watermark rides — a follower merges nothing, but the \
             projection is the same wire shape"
        );
    }

    /// The contract is **no value on the wire**, asserted where it matters:
    /// after a real encode→decode round trip, not on the in-memory struct.
    ///
    /// Three of the four also lose their *key* (they carry
    /// `skip_serializing_if`); `device_id` rides as an explicit `null` for the
    /// wire-compat reason in [`strip_for_public`]'s doc comment. Both halves are
    /// pinned below, so a future edit that flips either — dropping the
    /// `skip_serializing_if` on the three, or adding one to `device_id` and
    /// breaking older decoders — fails here.
    #[test]
    fn stripped_fields_carry_no_value_after_a_wire_round_trip() {
        let bytes = fauna_protocol::encode_canonical(&strip_for_public(
            wire_change_with_everything(),
            FolderReadGrant::Public,
        ))
        .expect("a change encodes");
        let decoded: SyncChange = fauna_protocol::decode_strict(&bytes).expect("and decodes");

        assert_eq!(decoded.device_id, None, "the owner's device fleet");
        assert_eq!(decoded.author_actor_id, None, "the authorship map");
        assert_eq!(decoded.path_sealed, None, "the sealed label + its salt");
        assert_eq!(decoded.content_key_version, None, "the M2 generation");
        assert_eq!(decoded.signature, None, "the writer signature");
        assert_eq!(decoded.signer_key, None, "the writer's device key");
        assert_eq!(
            decoded.path,
            Some("notes/a.md".to_string()),
            "the plaintext path is what a follower reads"
        );

        // dag-cbor writes map keys as literal text, so a surviving key appears
        // verbatim in the bytes.
        let contains = |haystack: &[u8], needle: &str| {
            haystack
                .windows(needle.len())
                .any(|w| w == needle.as_bytes())
        };
        for omitted in [
            "author_actor_id",
            "path_sealed",
            "content_key_version",
            "signature",
            "signer_key",
        ] {
            assert!(
                !contains(&bytes, omitted),
                "{omitted} carries skip_serializing_if — its KEY must not ride either"
            );
        }
        assert!(
            contains(&bytes, "device_id"),
            "device_id's key MUST still ride (as null): omitting it would fail \
             to decode on every peer without a matching #[serde(default)]"
        );

        // The control: an un-stripped row carries all four values, so the
        // assertions above witness the strip rather than a serializer that never
        // emits them.
        let full = fauna_protocol::encode_canonical(&wire_change_with_everything())
            .expect("a change encodes");
        let full: SyncChange = fauna_protocol::decode_strict(&full).expect("and decodes");
        assert!(full.device_id.is_some());
        assert!(full.author_actor_id.is_some());
        assert!(full.path_sealed.is_some());
        assert!(full.content_key_version.is_some());
    }
}
