//! Storage-group scopes — the T20 **recipient-set scheme**'s identity and
//! roster lattice.
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § The audience ladder
//! → *The recipient-set scheme* (roster, authority seam, lattice, membership
//! witness); key material: `key-material-hierarchy.md` § Audience: a storage
//! group; consumer: `docs/goal/behavior/p2p.md` § Offline share initiation.
//!
//! The scheme is a **generalization of two shipped patterns, never a third
//! invention** — the subscription `KeyBlob` scheme supplies the
//! period-key/per-recipient-wrap shape, and [`crate::generation`] supplies the
//! arbiter-free lattice. This module is the *sealing-agnostic* half: scope
//! identity, the roster records, their join, and the verified roster view.
//! Nothing here needs a key, which is exactly why it can land before the
//! group's key material is ruled.
//!
//! Two structural rules carry over from [`crate::generation`] and are the
//! reason this is a transplant rather than a rewrite:
//!
//! * **Per-writer monotone, `Removed` absorbing — within ONE writer's cell.**
//!   The roster join is [`crate::generation::join_device_set`]'s two-phase
//!   lattice at a different record type, so a removal can never be undone by
//!   a concurrent add. But the cell is **per (entry, author)** — logical key
//!   `<entry-id-hex>/<author-device-hex>`, the author being the device the
//!   row's own embedded carriage names ([`roster_cell_key`]) — the shape this
//!   plane's own revocation kind and the account plane's per-healer top-up
//!   cell already have (the per-writer roster cell ruling, 2026-09-27 —
//!   `account-data-taxonomy.md` § The recipient-set scheme → *The roster
//!   kind* → *The per-writer roster cell*). The plane key is held by every
//!   member ever admitted, of other accounts, so under a cell shared between
//!   writers the byte-order join had to keep one writer's bytes and drop
//!   another's: a self-consistent row under an attacker's own cert could
//!   displace the authority's, and an unsigned `Removed` severed any member
//!   for good. Now only the named device can write a row that verifies at
//!   its cell — the join ranks verifies-at-cell first
//!   ([`join_group_roster`]) and first-contact strictness refuses a row not
//!   verifying at its own recomputed key — so **no writer's row is ever
//!   displaced by another writer's bytes**, and the byte tie-break never
//!   crosses principals.
//! * **A `Removed` is AUTHORED and counts only on the authority chain.** It
//!   carries the same carriage and a signature over the entry id and the
//!   stamp ([`sign_roster_removal`]); the reader's fold per entry id is
//!   *excluded* iff some `Removed` cell verifies under a device cert chaining
//!   to the authority root — the remover's own standing not consulted: a
//!   removal is a stop-trusting signal, the revocation's class, and survives
//!   its author's revocation — else *enrolled* iff some `Enrolled` cell
//!   verifies under a device the line does not refuse
//!   ([`RosterView::build`]). A revoked device's `Enrolled` rows live in
//!   their own dead cells, refused by every reader that has the line and
//!   displacing nothing.
//! * **Re-admission of a REMOVED member mints a FRESH ENTRY ID.** The entry
//!   segment of a cell is the content-derived [`roster_entry_id`] of its
//!   [`RosterEntryCore`], whose `admission_salt` is fresh per admission, and
//!   an honored `Removed` excludes its entry id across every writer's cell.
//!   So "add-wins resurrection" is not merely refused — it is
//!   *unrepresentable*: a re-admitted member occupies a different entry id.
//!   (An authority-device severance is NOT such a re-admission: it
//!   re-publishes the SAME entry id under the live device's own cell, and
//!   writes no `Removed` — the revoked device's cell is dead on its own.)
//!
//! **The authority seam.** Membership authority in v1 is the *initiating
//! account's device fleet*: a roster entry is valid iff it is signed by a
//! device whose `DeviceAuthorization` chains to the authority actor root. That
//! is deliberately a seam — T19 reuses this scheme with authority set = the
//! box's admin set — so the root is a *parameter* of [`RosterView::build`],
//! never a hard-wired "the owner".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::data::DeviceAuthorization;
use crate::encoding::{EmbedAsBytes, canonical_decode, decode_signed_bytes, verify_envelope};
use crate::error::Error;
use crate::identity::ActorId;

/// Domain-separation context for a group scope's content-derived id.
/// **Frozen** — the id is the scope's name in every scope string, at-rest key
/// and admission verdict, so changing this orphans every group ever minted.
pub const GROUP_SCOPE_ID_CONTEXT: &str = "fauna.group-scope.id.v1 2026-08-17";

/// Domain-separation context for a roster entry's content-derived id.
/// **Frozen**, for the same reason: it is the entry's logical key.
pub const ROSTER_ENTRY_ID_CONTEXT: &str = "fauna.group-scope.roster-entry.id.v1 2026-08-17";

/// Domain-separation tag for the authority-device signature over one roster
/// entry (frozen — same replay discipline as
/// [`crate::generation::MINT_MINTER_SIG_CONTEXT`]).
pub const ROSTER_ENTRY_SIG_CONTEXT: &[u8] = b"fauna.group-scope.roster-entry.v1\0";

/// The birth record of a storage group scope — what its id commits to.
///
/// The scope id is content-derived from exactly this (the R14 (account-data-plane.md § The ratified decisions) key↔id
/// commitment pattern, [`crate::generation::generation_id`]), so a scope's
/// **name proves its authority**: nobody can mint a scope id that hashes to a
/// birth record naming an authority actor they do not hold, and a reader
/// handed a birth record re-derives the id rather than trusting a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupBirthRecord {
    /// The initiating account — the authority actor whose device fleet is the
    /// v1 authority set (module header: the authority seam).
    pub authority_actor: ActorId,
    /// Fresh random salt, so two groups born by the same authority in the
    /// same millisecond are still distinct scopes.
    #[serde(with = "serde_bytes")]
    pub salt: [u8; 32],
    /// The machinery root's commitment —
    /// `BLAKE3::derive_key(GROUP_MACHINERY_ROOT_COMMIT_CONTEXT, root)`
    /// ([`crate::crypto::GroupMachineryRoot::commitment`]). Because the scope
    /// id hashes over this record, the id **commits to the root**: a joiner
    /// unwraps the root from its admission bundle, re-derives the scope id it
    /// was offered, and refuses a root whose commitment does not match
    /// ([`verify_machinery_root_commitment`]) — so root substitution at
    /// admission is a crisp refusal at the joiner, never silent AEAD garbage,
    /// and two parties naming the same scope id provably hold the same
    /// machinery-reading capability.
    #[serde(with = "serde_bytes")]
    pub machinery_root_commit: [u8; 32],
    /// Birth stamp, unix ms. Advisory (clocks skew) — never merge-ordering.
    pub created_at_ms: i64,
}

/// The joiner-side commitment check: does `root` match the commitment this
/// birth record's scope id was derived over? Run it after unwrapping the
/// admission bundle and re-deriving the offered scope id — the two checks
/// together are what make a split-root scope unrepresentable (the field doc
/// above). The commitment is public data, so plain equality suffices.
#[must_use]
pub fn verify_machinery_root_commitment(
    birth: &GroupBirthRecord,
    root: &crate::crypto::GroupMachineryRoot,
) -> bool {
    root.commitment() == birth.machinery_root_commit
}

/// The content-derived scope id of a group: `BLAKE3` of the canonical encoding
/// of its birth record under [`GROUP_SCOPE_ID_CONTEXT`].
///
/// # Errors
/// Only on a record that fails canonical encoding — unreachable for honestly
/// constructed values.
pub fn group_scope_id(birth: &GroupBirthRecord) -> Result<[u8; 32], Error> {
    let bytes = crate::encoding::canonical_encode(birth)?;
    Ok(blake3::derive_key(GROUP_SCOPE_ID_CONTEXT, &bytes))
}

/// Decode a birth record handed over as `scope_id`'s, and hold it to that
/// claim: the record is returned only when its content-derived id IS
/// `scope_id`.
///
/// The one door every reader of a stored or served birth row goes through —
/// the scope fold every local reader shares, and the group plane's adopt, walk
/// and write. A birth record decodes to *an* authority whatever scope it is
/// filed under; only the re-derivation makes it *this* scope's. A reader that
/// skips it lets any root holder name themselves a scope's authority by
/// filing a record that hashes elsewhere.
///
/// # Errors
/// [`Error::Encoding`] when the bytes do not decode as a birth record, or when
/// the record's content-derived id is not `scope_id`.
pub fn decode_birth_for_scope(
    bytes: &[u8],
    scope_id: &[u8; 32],
) -> Result<GroupBirthRecord, Error> {
    let birth: GroupBirthRecord = canonical_decode(bytes)
        .map_err(|e| Error::Encoding(format!("group birth record does not decode: {e}")))?;
    if group_scope_id(&birth)? != *scope_id {
        return Err(Error::Encoding(
            "group birth record is not this scope's: its content-derived id is another scope's"
                .into(),
        ));
    }
    Ok(birth)
}

/// The identity core of one roster entry — what its logical key commits to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterEntryCore {
    /// The group this entry belongs to. Binding it into the id is what stops
    /// a valid entry from one group being replayed into another: the reader
    /// recomputes the id and refuses a core naming a different scope.
    #[serde(with = "serde_bytes")]
    pub scope_id: [u8; 32],
    /// The member's actor key — the identity the membership witness binds to
    /// the channel-proven peer (`account-data-plane.md` § The admission seam).
    pub member_actor: ActorId,
    /// Fresh per admission. This single field is what makes re-admission a
    /// different cell, and therefore what makes add-wins resurrection
    /// unrepresentable (module header).
    #[serde(with = "serde_bytes")]
    pub admission_salt: [u8; 32],
}

/// The content-derived id (and logical key) of a roster entry.
///
/// # Errors
/// Only on a core that fails canonical encoding.
pub fn roster_entry_id(core: &RosterEntryCore) -> Result<[u8; 32], Error> {
    let bytes = crate::encoding::canonical_encode(core)?;
    Ok(blake3::derive_key(ROSTER_ENTRY_ID_CONTEXT, &bytes))
}

/// The exact bytes an authority device signs for one roster entry — the
/// domain-separated content-derived entry id, which already commits to the
/// scope, the member actor and the admission salt, so this signature covers
/// everything the id covers with no second encoding of the core.
pub fn roster_entry_signing_bytes(entry_id: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ROSTER_ENTRY_SIG_CONTEXT.len() + 32);
    out.extend_from_slice(ROSTER_ENTRY_SIG_CONTEXT);
    out.extend_from_slice(entry_id);
    out
}

/// Sign one roster entry as an authority device.
pub fn sign_roster_entry(
    authority_device: &ed25519_dalek::SigningKey,
    entry_id: &[u8; 32],
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    authority_device
        .sign(&roster_entry_signing_bytes(entry_id))
        .to_bytes()
        .to_vec()
}

/// Domain-separation tag for the authority device's **binding** signature —
/// the one that ties the entry's reception key to the entry (frozen — the
/// [`ROSTER_ENTRY_SIG_CONTEXT`] replay discipline).
pub const ROSTER_BINDING_SIG_CONTEXT: &[u8] = b"fauna.group-scope.roster-binding.v1\0";

/// The exact bytes the authority device signs to BIND a reception key to one
/// roster entry: the domain tag, the fixed-width entry id (which already
/// commits to the scope, the member actor and the admission salt), the
/// big-endian stamp, and the length-prefixed reception key — fixed-width
/// throughout, so no field can be swapped under a carried-through signature.
/// [`roster_entry_signing_bytes`] covers only the id; before the binding
/// existed the wrap target beside it was unsigned (the bound roster entry
/// ruling, 2026-09-16 — `account-data-taxonomy.md` § The recipient-set scheme
/// → *The roster kind*).
#[must_use]
pub fn roster_binding_signing_bytes(
    entry_id: &[u8; 32],
    reception_pubkey: &[u8],
    enrolled_at_ms: i64,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(ROSTER_BINDING_SIG_CONTEXT.len() + 32 + 8 + 4 + reception_pubkey.len());
    out.extend_from_slice(ROSTER_BINDING_SIG_CONTEXT);
    out.extend_from_slice(entry_id);
    out.extend_from_slice(&enrolled_at_ms.to_be_bytes());
    out.extend_from_slice(
        &u32::try_from(reception_pubkey.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(reception_pubkey);
    out
}

/// Build one `Enrolled` roster record as an authority device — the ONE
/// production shape of an enrolment since the bound roster entry ruling:
/// the entry id is computed from `core`, and both the entry signature and
/// the binding signature are the same device's. `authorization` is the
/// canonical `EmbedAsBytes` carriage of that device's `DeviceAuthorization`.
/// Returns the entry id beside the record — the row's logical key.
///
/// # Errors
/// Only on a core that fails canonical encoding.
pub fn sign_roster_enrollment(
    authority_device: &ed25519_dalek::SigningKey,
    core: RosterEntryCore,
    reception_pubkey: Vec<u8>,
    authorization: Vec<u8>,
    enrolled_at_ms: i64,
) -> Result<([u8; 32], GroupRosterRecord), Error> {
    use ed25519_dalek::Signer;
    let entry_id = roster_entry_id(&core)?;
    let binding_sig = authority_device
        .sign(&roster_binding_signing_bytes(
            &entry_id,
            &reception_pubkey,
            enrolled_at_ms,
        ))
        .to_bytes()
        .to_vec();
    Ok((
        entry_id,
        GroupRosterRecord::Enrolled {
            core,
            reception_pubkey,
            authorization,
            authority_sig: sign_roster_entry(authority_device, &entry_id),
            enrolled_at_ms,
            binding_sig,
        },
    ))
}

/// Domain-separation tag for an authored roster **removal**'s signature
/// (frozen — the [`ROSTER_ENTRY_SIG_CONTEXT`] replay discipline; a sibling of
/// it, never a reuse, so an entry signature can never be replayed as a
/// removal or the reverse).
pub const ROSTER_REMOVAL_SIG_CONTEXT: &[u8] = b"fauna.group-scope.roster-removal.v1\0";

/// The exact bytes an authority device signs to REMOVE one roster entry: the
/// domain tag, the fixed-width entry id (which already commits to the scope,
/// the member actor and the admission salt) and the big-endian stamp — no
/// variable-length field, so nothing can be swapped under a carried-through
/// signature.
#[must_use]
pub fn roster_removal_signing_bytes(entry_id: &[u8; 32], removed_at_ms: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(ROSTER_REMOVAL_SIG_CONTEXT.len() + 32 + 8);
    out.extend_from_slice(ROSTER_REMOVAL_SIG_CONTEXT);
    out.extend_from_slice(entry_id);
    out.extend_from_slice(&removed_at_ms.to_be_bytes());
    out
}

/// Build one `Removed` roster record as an authority device — the ONE
/// production shape of a removal: authored (the device's carriage rides the
/// row) and signed over the entry id and the stamp. Returns the row's
/// logical key — the device's own cell at that entry — beside the record.
#[must_use]
pub fn sign_roster_removal(
    authority_device: &ed25519_dalek::SigningKey,
    entry_id: [u8; 32],
    authorization: Vec<u8>,
    removed_at_ms: i64,
) -> (String, GroupRosterRecord) {
    use ed25519_dalek::Signer;
    let remover_sig = authority_device
        .sign(&roster_removal_signing_bytes(&entry_id, removed_at_ms))
        .to_bytes()
        .to_vec();
    (
        roster_cell_key(&entry_id, &authority_device.verifying_key().to_bytes()),
        GroupRosterRecord::Removed {
            entry_id,
            authorization,
            removed_at_ms,
            remover_sig,
        },
    )
}

/// The two-segment roster cell key `<entry-id-hex>/<author-device-hex>` —
/// one cell per (entry, author). One derivation, shared by every writer, the
/// view, the adoption arm and the tests — never re-formatted ad hoc.
#[must_use]
pub fn roster_cell_key(entry_id: &[u8; 32], author_device: &[u8; 32]) -> String {
    format!(
        "{}/{}",
        crate::hex32::encode(entry_id),
        crate::hex32::encode(author_device)
    )
}

/// Parse a roster logical key — canonical-or-nothing, exactly two segments
/// (the [`crate::generation::parse_wrap_cell_key`] injectivity rule).
#[must_use]
pub fn parse_roster_cell_key(key: &str) -> Option<([u8; 32], [u8; 32])> {
    let canonical = |segment: &str| -> Option<[u8; 32]> {
        let bytes = crate::hex32::decode(segment).ok()?;
        (crate::hex32::encode(&bytes) == segment).then_some(bytes)
    };
    let (entry, author) = key.split_once('/')?;
    Some((canonical(entry)?, canonical(author)?))
}

/// One writer's row in a group scope's `fauna.group.roster`, at the
/// two-segment logical key [`roster_cell_key`] — the content-derived
/// [`roster_entry_id`] of its core and the authoring device the row's own
/// carriage names.
///
/// The device-set lattice transplanted, per writer: within one cell `Removed`
/// absorbs, and the enrollment payload deliberately does not survive the join
/// — a removed member's wrap target is dead, and historical membership lives
/// in the mint DAG. Across writers nothing absorbs anything: the reader folds
/// the cells of one entry id under the authority line ([`RosterView::build`]).
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupRosterRecord {
    /// An enrolled member — a wrap target for every generation minted while
    /// this entry stands.
    Enrolled {
        /// The identity core this row's logical key commits to.
        core: RosterEntryCore,
        /// The member's **group-reception X-Wing public key as observed at
        /// add**. Deliberately a snapshot, not a pointer: the scheme rotates
        /// wrap targets through *re-mints* triggered by the member's own
        /// published rotation, so a generation's wrap set is a fact about the
        /// keys that existed when it was minted.
        #[serde(with = "serde_bytes")]
        reception_pubkey: Vec<u8>,
        /// The authority device's `DeviceAuthorization`, chaining to the
        /// authority actor root: canonical dag-cbor of an [`EmbedAsBytes`]
        /// `{envelope, bytes}` pair — the same self-contained carriage every
        /// witness surface uses, so a verifier needs no registry lookup.
        #[serde(with = "serde_bytes")]
        authorization: Vec<u8>,
        /// That device's Ed25519 signature over
        /// [`roster_entry_signing_bytes`] of the **recomputed** entry id.
        #[serde(with = "serde_bytes")]
        authority_sig: Vec<u8>,
        /// Enrollment stamp, unix ms. Advisory — the lattice orders, stamps
        /// inform.
        enrolled_at_ms: i64,
        /// The authority device's **binding** signature over
        /// [`roster_binding_signing_bytes`]: the entry id, the stamp and the
        /// reception key. `authority_sig` covers only the id, so without this
        /// the wrap target was unsigned and any group-plane key holder could
        /// re-file a member's entry beside its own reception key and, winning
        /// the byte-order join, have every later group mint and top-up seal
        /// to itself under the member's name (the bound roster entry ruling,
        /// 2026-09-16 — the device-set self-signed enrollment's twin).
        /// **Required by every reader** since the compat-remnant sweep
        /// (2026-09-25, program 4): the empty shape pre-ruling binaries wrote
        /// — which no writer has produced since — is refused by the shared
        /// verifier behind [`RosterView::build`] and the admission witness
        /// door, never chain-verified. The serde attributes are the additive
        /// field discipline, not an accept path: a decoder tolerates the
        /// absence (the row stays a lattice element the join can rank and a
        /// removal can absorb) and the verifier refuses it. [`join_group_roster`]
        /// still ranks an entry whose binding verifies under its own embedded
        /// cert above any that does not, so a forgery never displaces the
        /// authority's row at rest.
        #[serde(default, skip_serializing_if = "Vec::is_empty", with = "serde_bytes")]
        binding_sig: Vec<u8>,
    },
    /// The absorbing phase of this writer's cell — and, once it verifies
    /// under a device cert chaining to the authority root, the fold's
    /// exclusion of the whole entry id: severed from every generation minted
    /// after the removal merges, whichever writer's `Enrolled` cells stand at
    /// that id. The member keeps what it could already open (the universal
    /// rule-6 bound) and may be re-admitted only under a *new* entry id.
    ///
    /// **Authored, never unconditional** — the transplant's reason for an
    /// unconditional `Removed` (every plane-key holder is the user's own
    /// device) does not hold here, where the key is every ever-admitted
    /// member's. A `Removed` whose chain does not verify excludes nobody
    /// ([`RosterView::build`]). **And a stop-trusting signal, like a
    /// revocation:** the remover's own standing is not consulted, so a
    /// removal outlives its author's revocation — revoking the device that
    /// removed a member never resurrects the member (*A removal survives
    /// its author's revocation*, ruled 2026-09-27).
    Removed {
        /// The removed entry id — echoes cell segment 1 (there is no core to
        /// recompute it from), and is what the signature covers.
        #[serde(with = "serde_bytes")]
        entry_id: [u8; 32],
        /// The removing authority device's `DeviceAuthorization` in the
        /// `EmbedAsBytes` carriage every witness surface uses; names the
        /// device the cell's segment 2 must echo.
        #[serde(with = "serde_bytes")]
        authorization: Vec<u8>,
        /// Removal stamp, unix ms. Advisory, and the instant the remover's
        /// cert expiry anchors to.
        removed_at_ms: i64,
        /// Ed25519 by the authorized device over
        /// [`roster_removal_signing_bytes`].
        #[serde(with = "serde_bytes")]
        remover_sig: Vec<u8>,
    },
}

impl GroupRosterRecord {
    /// The authority device this row's OWN embedded `authorization` names —
    /// decoded, never chain-verified (the chain is the reader's business:
    /// [`RosterView::build`] and the admission witness door). `None` for a
    /// carriage that does not decode. This is the row's **author**: the
    /// device whose cell it must sit in ([`roster_cell_key`]).
    ///
    /// The one question a **re-binding** authority device must ask before it
    /// signs: a binding is only ever checked against the key the row's own
    /// carriage names ([`Self::self_verifies`] reads exactly this), so a
    /// signature by any other device is unverifiable noise — a sibling
    /// authority device's row is that sibling's to re-publish, never ours.
    #[must_use]
    pub fn authority_device_key(&self) -> Option<[u8; 32]> {
        match self {
            GroupRosterRecord::Enrolled { authorization, .. }
            | GroupRosterRecord::Removed { authorization, .. } => {
                embedded_cert_device_key(authorization)
            }
        }
    }

    /// The entry id this row speaks for: recomputed from the core of an
    /// `Enrolled` (never taken on trust), the signed field of a `Removed`.
    /// `None` only for a core that fails canonical encoding.
    #[must_use]
    pub fn entry_id(&self) -> Option<[u8; 32]> {
        match self {
            GroupRosterRecord::Enrolled { core, .. } => roster_entry_id(core).ok(),
            GroupRosterRecord::Removed { entry_id, .. } => Some(*entry_id),
        }
    }

    /// Does this row **self-verify** — its signature(s) verifying, under the
    /// device key its OWN embedded `authorization` names, over what the row
    /// asserts? For an `Enrolled`: the binding over the entry id recomputed
    /// from its own core, its stamp and its reception key. For a `Removed`:
    /// the remover signature over its entry id and stamp. Pure over the
    /// record alone (a merge may never consult external state; the entry id
    /// is inside the record, so no cell key is needed). `false` for an entry
    /// with no binding (refused at every reader; ranked lowest here) and for
    /// a cert that does not even decode.
    ///
    /// Self-consistency is the most a join can check: an attacker can make a
    /// row self-consistent under a cert of its OWN — such a row then fails
    /// the authority chain at every reader ([`RosterView::build`], the
    /// admission witness door) and, under the per-writer cell, sits in the
    /// attacker's own cell where it displaces nobody's bytes. What it cannot
    /// do is produce the authority device's signature over a member's row.
    #[must_use]
    pub fn self_verifies(&self) -> bool {
        let Some(device_key) = self.authority_device_key() else {
            return false;
        };
        match self {
            GroupRosterRecord::Enrolled {
                core,
                reception_pubkey,
                enrolled_at_ms,
                binding_sig,
                ..
            } => {
                let Ok(entry_id) = roster_entry_id(core) else {
                    return false;
                };
                binding_sig.len() == 64
                    && crate::identity::verify_detached(
                        &device_key,
                        &roster_binding_signing_bytes(&entry_id, reception_pubkey, *enrolled_at_ms),
                        binding_sig,
                    )
            }
            GroupRosterRecord::Removed {
                entry_id,
                removed_at_ms,
                remover_sig,
                ..
            } => {
                remover_sig.len() == 64
                    && crate::identity::verify_detached(
                        &device_key,
                        &roster_removal_signing_bytes(entry_id, *removed_at_ms),
                        remover_sig,
                    )
            }
        }
    }

    /// Does this row verify **at its cell** — its recomputed entry id echoing
    /// cell segment 1, its own carriage naming cell segment 2, and
    /// [`Self::self_verifies`]? The roster kind's first-contact contract (the
    /// revocation kind's `verifies_at` shape): only the named device can
    /// write a self-verifying row into its cell. Pure over the record and the
    /// cell coordinates; whether the author speaks for the authority is
    /// [`RosterView::build`]'s line-consulting half.
    #[must_use]
    pub fn verifies_at(&self, cell_entry: &[u8; 32], cell_author: &[u8; 32]) -> bool {
        self.entry_id() == Some(*cell_entry)
            && self.authority_device_key() == Some(*cell_author)
            && self.self_verifies()
    }
}

/// The device key an `authorization` carriage names — decoded, never
/// verified (the join's self-consistency check needs the key; the chain is
/// the reader's business).
fn embedded_cert_device_key(authorization: &[u8]) -> Option<[u8; 32]> {
    let wire: EmbedAsBytes = canonical_decode(authorization).ok()?;
    let (cert_bytes, _) = wire.into_signed().ok()?;
    let cert: DeviceAuthorization = decode_signed_bytes(&cert_bytes).ok()?;
    Some(cert.device_key)
}

/// The `fauna.group.roster` join at one cell, byte-level and **key-aware** —
/// the revocation kind's shape ([`join_authority_revocation`]): rank =
/// (verifies at THIS cell — [`GroupRosterRecord::verifies_at`], `Removed`,
/// bytes), lexicographic max, the winning input returned verbatim; a side
/// that does not decode is an `Err`, never ranked (the
/// [`crate::generation::join_generation_wrap_per_healer`] decode-or-fail
/// contract — a merge never ranks a value it cannot decode). Within a
/// writer's cell its own `Removed` absorbs its own `Enrolled`; a row that
/// does not verify at the cell — another writer's self-verifying row filed
/// here, an unbound row, a forgery under a carried binding, a removal with a
/// broken signature; none of which counts at a reader — ranks below any that
/// does, so **a standing row is never displaced by bytes from outside its
/// own writer's cell**, and two non-verifying rows still break ties by byte
/// order. First-contact strictness (`fauna_protocol::merge_policy`) refuses
/// such a row outright when no row stands yet.
///
/// Found at build: the ruling first wrote
/// this join key-free, on the argument that the cell is the writer's own; but
/// a merge into a STANDING cell runs the join, not the adoption arm, so a
/// self-verifying row from another writer's cell won a standing cell on
/// bytes — the displacement the cell exists to prevent. The key is what the
/// two rows do not carry: which writer's cell this is.
pub fn join_group_roster<'a>(
    cell_entry: &[u8; 32],
    cell_author: &[u8; 32],
    current: &'a [u8],
    incoming: &'a [u8],
) -> Result<&'a [u8], Error> {
    let rank = |bytes: &[u8]| -> Result<(bool, bool), Error> {
        let rec = canonical_decode::<GroupRosterRecord>(bytes)?;
        Ok((
            rec.verifies_at(cell_entry, cell_author),
            matches!(rec, GroupRosterRecord::Removed { .. }),
        ))
    };
    let (cur, inc) = (rank(current)?, rank(incoming)?);
    Ok(if (cur, current) >= (inc, incoming) {
        current
    } else {
        incoming
    })
}

// ── The authority line — and how a reader learns an authority device's
// revocation ──────────────────────────────────────────────────────────────────
//
// Ruled 2026-09-19;
// owner `account-data-taxonomy.md` § The recipient-set scheme →
// *Severance, per axis* (the authority-device axis). The chain check alone
// accepts ANY device the authority root ever certified, and a device removed
// from the authority's own account keeps its device secret, its root-signed
// cert and the never-rotated machinery root — so without a revocation input it
// keeps authoring roster entries, bindings and mints for ever. The authority's
// account plane (where `FleetView` excludes the removed id) is sealed to the
// authority's own fleet: a cross-account member can never read it. So the
// fact is published INTO the group plane, as this kind, and every authority
// check on the plane reads it through [`GroupAuthority`] — the one
// implementation behind the roster view, the membership witness and the mint
// resolver.

/// Domain-separation tag for an authority-device revocation's signature
/// (frozen — the [`ROSTER_ENTRY_SIG_CONTEXT`] replay discipline).
pub const AUTHORITY_REVOCATION_SIG_CONTEXT: &[u8] = b"fauna.group-scope.authority-revocation.v1\0";

/// One row of `fauna.group.authority-revocation`, at the two-segment logical
/// key `"<revoked-device-hex>/<revoker-hex>"` — one cell per (revoked device,
/// revoker), so each revoker owns its own cell and nobody's revocation can be
/// displaced by another writer's bytes (the per-healer top-up cell shape).
///
/// **Authored, never unconditional** — the one deliberate departure from the
/// device-set lattice this plane otherwise transplants. There, an
/// unconditional `Removed` is fail-safe because every plane-key holder is the
/// user's own device. Here the plane key (the machinery root) is held by every
/// member ever admitted, of OTHER accounts, so an unconditional revocation
/// would let any former member kill the group's authority. A revocation
/// therefore counts only when signed by the authority root (or a prior
/// identity), or by a device whose cert chains to it.
///
/// **The revoker's own standing is deliberately NOT consulted.** "Was the
/// revoker still un-revoked?" reads non-monotone merged state — two replicas
/// meeting a mutual-revocation race in opposite orders would refuse opposite
/// rows and diverge (the `FleetView` ruling's first reason, unchanged). So a
/// mutual race revokes both: honoring a "stop trusting" signal eagerly is the
/// fail-safe direction, and the vandal set is exactly the account plane's
/// accepted one — the authority account's own ever-certified devices.
///
/// An enum from birth so a later variant lands additively.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorityRevocationRecord {
    /// `device` is no longer an authority device of `scope_id`.
    Revoked {
        /// The group this revocation speaks for — signed, so a row never
        /// replays into another scope's plane.
        #[serde(with = "serde_bytes")]
        scope_id: [u8; 32],
        /// The revoked authority device id — echoes cell segment 1.
        #[serde(with = "serde_bytes")]
        device: [u8; 32],
        /// The revoking key — echoes cell segment 2, and IS the Ed25519
        /// verification key for `revoker_sig`: an authority device id, or the
        /// authority actor root itself (then `authorization` is empty).
        #[serde(with = "serde_bytes")]
        revoker: [u8; 32],
        /// The revoking device's `DeviceAuthorization` in the `EmbedAsBytes`
        /// carriage every witness surface uses; empty when the revoker is the
        /// authority root or a prior identity.
        #[serde(default, skip_serializing_if = "Vec::is_empty", with = "serde_bytes")]
        authorization: Vec<u8>,
        /// Revocation stamp, unix ms. Advisory, and the instant the revoker's
        /// cert expiry anchors to — never an ordering: a revocation is total
        /// over the device's signatures (see [`GroupAuthority`]).
        revoked_at_ms: i64,
        /// Ed25519 by `revoker` over [`authority_revocation_signing_bytes`].
        #[serde(with = "serde_bytes")]
        revoker_sig: Vec<u8>,
    },
}

/// The exact bytes a revoker signs: the domain tag, then the fixed-width
/// scope, revoked device and revoker ids, then the big-endian stamp — no
/// variable-length field, so nothing can be swapped under a carried-through
/// signature.
#[must_use]
pub fn authority_revocation_signing_bytes(
    scope_id: &[u8; 32],
    device: &[u8; 32],
    revoker: &[u8; 32],
    revoked_at_ms: i64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(AUTHORITY_REVOCATION_SIG_CONTEXT.len() + 32 * 3 + 8);
    out.extend_from_slice(AUTHORITY_REVOCATION_SIG_CONTEXT);
    out.extend_from_slice(scope_id);
    out.extend_from_slice(device);
    out.extend_from_slice(revoker);
    out.extend_from_slice(&revoked_at_ms.to_be_bytes());
    out
}

/// Build one revocation row as `revoker` — the ONE production shape. Pass the
/// revoking device's signing key with its cert carriage, or the authority
/// actor's own signing key with an empty `authorization`. Returns the row's
/// logical key beside the record.
#[must_use]
pub fn sign_authority_revocation(
    revoker: &ed25519_dalek::SigningKey,
    authorization: Vec<u8>,
    scope_id: [u8; 32],
    device: [u8; 32],
    revoked_at_ms: i64,
) -> (String, AuthorityRevocationRecord) {
    use ed25519_dalek::Signer;
    let revoker_id = revoker.verifying_key().to_bytes();
    let revoker_sig = revoker
        .sign(&authority_revocation_signing_bytes(
            &scope_id,
            &device,
            &revoker_id,
            revoked_at_ms,
        ))
        .to_bytes()
        .to_vec();
    (
        authority_revocation_cell_key(&device, &revoker_id),
        AuthorityRevocationRecord::Revoked {
            scope_id,
            device,
            revoker: revoker_id,
            authorization,
            revoked_at_ms,
            revoker_sig,
        },
    )
}

impl AuthorityRevocationRecord {
    /// Does this record verify **at its cell** — both ids echoing the cell
    /// key's segments and `revoker_sig` verifying under the cell's revoker
    /// over exactly these fields? Pure over the record and the cell
    /// coordinates (a merge may never consult external state); whether the
    /// revoker speaks for the authority is [`GroupAuthority::build`]'s
    /// second, line-consulting half.
    #[must_use]
    pub fn verifies_at(&self, cell_device: &[u8; 32], cell_revoker: &[u8; 32]) -> bool {
        let AuthorityRevocationRecord::Revoked {
            scope_id,
            device,
            revoker,
            revoked_at_ms,
            revoker_sig,
            ..
        } = self;
        device == cell_device
            && revoker == cell_revoker
            && revoker_sig.len() == 64
            && crate::identity::verify_detached(
                revoker,
                &authority_revocation_signing_bytes(scope_id, device, revoker, *revoked_at_ms),
                revoker_sig,
            )
    }
}

/// The two-segment revocation cell key. One derivation, shared by the writer,
/// the view and the tests — never re-formatted ad hoc.
#[must_use]
pub fn authority_revocation_cell_key(device: &[u8; 32], revoker: &[u8; 32]) -> String {
    format!(
        "{}/{}",
        crate::hex32::encode(device),
        crate::hex32::encode(revoker)
    )
}

/// Parse a revocation logical key — canonical-or-nothing, exactly two
/// segments (the [`crate::generation::parse_wrap_cell_key`] injectivity rule).
#[must_use]
pub fn parse_authority_revocation_cell_key(key: &str) -> Option<([u8; 32], [u8; 32])> {
    let canonical = |segment: &str| -> Option<[u8; 32]> {
        let bytes = crate::hex32::decode(segment).ok()?;
        (crate::hex32::encode(&bytes) == segment).then_some(bytes)
    };
    let (device, revoker) = key.split_once('/')?;
    Some((canonical(device)?, canonical(revoker)?))
}

/// The `fauna.group.authority-revocation` join — byte-level, decode-or-fail:
/// rank = (verifies-at-this-cell, bytes), lexicographic max, winner returned
/// verbatim. The top-up kind's contract (a side that does not decode is an
/// `Err`, never ranked; first-contact strictness lives in the adoption arm).
/// No stamp in the rank: a revocation has no fresher form, only a present one.
pub fn join_authority_revocation<'a>(
    device: &[u8; 32],
    revoker: &[u8; 32],
    current: &'a [u8],
    incoming: &'a [u8],
) -> Result<&'a [u8], Error> {
    let rank = |bytes: &[u8]| -> Result<bool, Error> {
        Ok(canonical_decode::<AuthorityRevocationRecord>(bytes)?.verifies_at(device, revoker))
    };
    let (cur, inc) = (rank(current)?, rank(incoming)?);
    Ok(if (cur, current) >= (inc, incoming) {
        current
    } else {
        incoming
    })
}

/// A group scope's **authority line as one reader sees it**: the authority
/// root, its prior (succeeded-from) identities, and the authority devices the
/// reader has learned are revoked. Every authority check on the plane — the
/// roster view, the membership witness, the mint resolver, an authored shred —
/// goes through [`Self::verify_device_cert`], so none can be built without a
/// revocation input and no two can answer to different authorities.
///
/// **A revocation is total, never dated.** The plane has no arbiter, so there
/// is no "before the revocation": stamps are advisory and the revoked device
/// chooses its own. Every signature of a revoked device is therefore refused,
/// whenever it claims to have been made. What the authority still vouches
/// for, a live authority device re-publishes under its own carriage — the
/// same entry id, in its own cell; the revoked device's cell is dead on its
/// own at every reader that has the line — and the line-committed
/// admissibility of the mint kind (`GroupMintCore::revoked_past`) is what
/// makes a tip minted before the revocation inadmissible, so the severance
/// mint re-keys the group past the revoked device, exactly as an
/// authority-device compromise deserves.
///
/// Built from merged plane rows, so — like [`RosterView`] — every verdict is a
/// deterministic function of converged state. Unlike `prior`, the revoked set
/// moves verdicts toward REFUSAL; it only ever grows (the kind has no removing
/// phase), so the view stays monotone in that direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupAuthority {
    root: ActorId,
    prior: Vec<ActorId>,
    revoked: std::collections::BTreeSet<[u8; 32]>,
    invalid: Vec<(String, String)>,
}

impl GroupAuthority {
    /// Fold the scope's merged `fauna.group.authority-revocation` rows —
    /// `(logical key, merged value bytes)` — into the authority line.
    ///
    /// A row counts when its key is canonical, it verifies at its cell, it
    /// names this scope, and its revoker is the authority root, a prior
    /// identity, or a device whose cert chains to one (expiry anchored to the
    /// revocation's asserted instant). Anything else is [`Self::invalid`]:
    /// loudly countable, never a revocation. The revoker's own revocation is
    /// not consulted (the record's ruling).
    ///
    /// A reader with no revocation rows passes an empty iterator — there is
    /// deliberately no constructor that omits the argument.
    pub fn build<'a, I>(
        scope_id: &[u8; 32],
        root: &ActorId,
        prior: &[ActorId],
        revocation_rows: I,
    ) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a [u8])>,
    {
        let mut line = GroupAuthority {
            root: *root,
            prior: prior.to_vec(),
            revoked: std::collections::BTreeSet::new(),
            invalid: Vec::new(),
        };
        for (key, value) in revocation_rows {
            match line.verify_revocation_row(scope_id, key, value) {
                Ok(device) => {
                    line.revoked.insert(device);
                }
                Err(reason) => line.invalid.push((key.to_string(), reason)),
            }
        }
        line
    }

    fn verify_revocation_row(
        &self,
        scope_id: &[u8; 32],
        key: &str,
        value: &[u8],
    ) -> Result<[u8; 32], String> {
        let (cell_device, cell_revoker) = parse_authority_revocation_cell_key(key)
            .ok_or("revocation key is not two canonical hex segments")?;
        let record: AuthorityRevocationRecord =
            canonical_decode(value).map_err(|e| format!("revocation does not decode: {e}"))?;
        if !record.verifies_at(&cell_device, &cell_revoker) {
            return Err("revocation does not verify at its cell".to_string());
        }
        let AuthorityRevocationRecord::Revoked {
            scope_id: named_scope,
            authorization,
            revoked_at_ms,
            ..
        } = &record;
        if named_scope != scope_id {
            return Err("revocation names a different group scope".to_string());
        }
        let revoker_is_root =
            cell_revoker == self.root.0 || self.prior.iter().any(|p| p.0 == cell_revoker);
        if !revoker_is_root {
            let cert = self.verify_chain("revoker authorization", authorization, *revoked_at_ms)?;
            if cert.device_key != cell_revoker {
                return Err("revoker authorization covers a different device id".to_string());
            }
        }
        Ok(cell_device)
    }

    /// The authority actor root.
    #[must_use]
    pub fn root(&self) -> &ActorId {
        &self.root
    }

    /// Has this reader learned that `device` is revoked?
    #[must_use]
    pub fn is_revoked(&self, device: &[u8; 32]) -> bool {
        self.revoked.contains(device)
    }

    /// The revoked authority device ids, ascending.
    pub fn revoked(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.revoked.iter()
    }

    /// Revocation rows that verified as nothing: `(row key, reason)`.
    #[must_use]
    pub fn invalid(&self) -> &[(String, String)] {
        &self.invalid
    }

    /// **The one authority-device check.** `authorization` must be a valid
    /// `DeviceAuthorization` carriage signed by the authority root or a prior
    /// identity (never a device — "a device cannot authorize a device",
    /// enforced structurally by that allow-list), not expired before
    /// `asserted_at_ms`, and covering a device this reader has not learned is
    /// revoked. Returns the authorized device key, which the caller then
    /// verifies its own signature under. `what` names the carriage in the
    /// refusal text.
    ///
    /// # Errors
    /// A malformed, foreign-rooted, expired or revoked authorization.
    pub fn verify_device_cert(
        &self,
        what: &str,
        authorization: &[u8],
        asserted_at_ms: i64,
    ) -> Result<[u8; 32], String> {
        let cert = self.verify_chain(what, authorization, asserted_at_ms)?;
        if self.is_revoked(&cert.device_key) {
            return Err(format!("{what} covers a revoked authority device"));
        }
        Ok(cert.device_key)
    }

    /// **The chain half alone — what a STOP-TRUSTING signal's author is held
    /// to.** A revoker ([`AuthorityRevocationRecord`], rule (2)) and a
    /// roster remover ([`RosterView::build`]'s `Removed` fold — *A removal
    /// survives its author's revocation*, ruled 2026-09-27) must chain to
    /// the authority root or a prior identity, unexpired at the asserted
    /// instant; their own standing is deliberately NOT consulted, so the
    /// signal outlives its author's revocation and the refused set only ever
    /// grows. Every *positive* signal goes through
    /// [`Self::verify_device_cert`] instead. Returns the authorized device
    /// key, which the caller then verifies its own signature under.
    ///
    /// # Errors
    /// A malformed, foreign-rooted or expired authorization.
    pub fn verify_device_chain(
        &self,
        what: &str,
        authorization: &[u8],
        asserted_at_ms: i64,
    ) -> Result<[u8; 32], String> {
        self.verify_chain(what, authorization, asserted_at_ms)
            .map(|cert| cert.device_key)
    }

    fn verify_chain(
        &self,
        what: &str,
        authorization: &[u8],
        asserted_at_ms: i64,
    ) -> Result<DeviceAuthorization, String> {
        let wire: EmbedAsBytes = canonical_decode(authorization)
            .map_err(|e| format!("{what} is not an EmbedAsBytes carriage: {e}"))?;
        let (cert_bytes, cert_env) = wire
            .into_signed()
            .map_err(|e| format!("{what} envelope malformed: {e}"))?;
        let cert: DeviceAuthorization = decode_signed_bytes(&cert_bytes)
            .map_err(|e| format!("{what} bytes are not a DeviceAuthorization: {e}"))?;
        verify_envelope(&cert, &cert_bytes, &cert_env)
            .map_err(|_| format!("{what} signature invalid"))?;
        if cert.actor_id != self.root && !self.prior.contains(&cert.actor_id) {
            return Err(format!(
                "{what} is signed by neither the authority root nor a prior identity"
            ));
        }
        // Expiry anchors to the asserted instant (the authoring-twin
        // pattern). Cert timestamps are seconds; plane stamps are millis.
        if let Some(expires_at) = cert.expires_at
            && expires_at.0.saturating_mul(1_000) < u64::try_from(asserted_at_ms).unwrap_or(0)
        {
            return Err(format!("{what} expired before the asserted instant"));
        }
        Ok(cert)
    }
}

/// One verified roster member — a wrap target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterMember {
    /// The entry id (= this member's roster cell).
    pub entry_id: [u8; 32],
    /// The member's actor key.
    pub member_actor: ActorId,
    /// The group-reception X-Wing public key a minter wraps to.
    pub reception_pubkey: Vec<u8>,
    /// The enrollment's asserted instant, unix ms (advisory).
    pub enrolled_at_ms: i64,
}

/// The verified reading of one group scope's merged roster rows — the
/// [`crate::generation::FleetView`] twin.
///
/// Build it from **merged plane state** (one row per (entry, author) cell)
/// plus the group's birth facts; every verdict is a deterministic function of
/// those inputs — no clock, no arrival order, no local preference — so two
/// replicas with converged rows hold identical views. That determinism is
/// what lets mint admissibility be *roster coverage* with no arbiter anywhere.
///
/// **The fold, per entry id, under the authority line:** *excluded* iff some
/// `Removed` cell verifies under a device cert chaining to the authority
/// root (the remover's own standing not consulted — a removal is a
/// stop-trusting signal and survives its author's revocation), else
/// *enrolled* iff some `Enrolled` cell verifies under a device the line does
/// not refuse. A pure function of the merged cells and the line, so two
/// replicas that merge the same rows in opposite orders fold alike, and
/// exclusion is monotone under a growing line; the `FleetView`
/// non-monotonicity argument concerned a precondition evaluated against
/// merged state at ingest and stored, which this fold never does. What it
/// closes: every cross-writer displacement — a revoked device's `Enrolled`
/// lives in its own dead cell, refused here and displacing nothing; a
/// stranger's `Removed` excludes nobody.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RosterView {
    members: BTreeMap<[u8; 32], RosterMember>,
    removed: BTreeMap<[u8; 32], ()>,
    invalid: Vec<(String, String)>,
}

impl RosterView {
    /// Fold merged roster rows — `(logical key, merged value bytes)` — into the
    /// verified view.
    ///
    /// `scope_id` is the group being read and `authority` its authority line
    /// as this reader sees it ([`GroupAuthority`]): the birth record's
    /// authority actor, that actor's succeeded-from identities (which is what
    /// lets an entry signed before a succession keep verifying) and the
    /// authority devices learned revoked. `prior` only ever grows and flips
    /// verdicts toward acceptance; the revoked set only ever grows and flips
    /// them toward refusal — each monotone, and identical on every replica
    /// that learned the same facts.
    ///
    /// A row whose key is not the canonical two-segment [`roster_cell_key`]
    /// of its recomputed entry id and its own carriage's device, whose value
    /// does not decode, whose core names another scope, or whose authority
    /// chain does not verify is flagged [`Self::invalid`]: loudly countable,
    /// never a member and never an exclusion.
    ///
    /// Several honored `Enrolled` cells at one entry id (a live sibling's
    /// re-publication beside the original writer's) enrol one member: the
    /// newest by stamp, ties by author device — a deterministic choice among
    /// rows the line vouches for alike, and the one that lets a later
    /// re-publication carry a member's rotated key.
    pub fn build<'a, I>(scope_id: &[u8; 32], authority: &GroupAuthority, rows: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a [u8])>,
    {
        let mut view = RosterView::default();
        // (stamp, author) → member, per entry id: the fold's inputs before
        // the exclusions are applied.
        let mut enrolled: BTreeMap<[u8; 32], (i64, [u8; 32], RosterMember)> = BTreeMap::new();
        for (key, value) in rows {
            let Some((cell_entry, cell_author)) = parse_roster_cell_key(key) else {
                view.invalid.push((
                    key.to_string(),
                    "row key is not two canonical hex32 segments (<entry>/<author>)".to_string(),
                ));
                continue;
            };
            let record: GroupRosterRecord = match canonical_decode(value) {
                Ok(r) => r,
                Err(e) => {
                    view.invalid
                        .push((key.to_string(), format!("value does not decode: {e}")));
                    continue;
                }
            };
            match record {
                GroupRosterRecord::Enrolled {
                    core,
                    reception_pubkey,
                    authorization,
                    authority_sig,
                    enrolled_at_ms,
                    binding_sig,
                } => {
                    let verified = verify_enrolled_entry_parts(
                        &core,
                        &reception_pubkey,
                        &authorization,
                        &authority_sig,
                        enrolled_at_ms,
                        &binding_sig,
                        scope_id,
                        authority,
                    );
                    match verified {
                        // The row must also sit at its OWN cell: the entry
                        // segment its recomputed id, the author segment the
                        // device its own carriage names. This is the one
                        // check the carried-witness path has no use for (a
                        // witness carries no row key), which is why it lives
                        // here rather than inside the shared verification.
                        Ok((member, _)) if member.entry_id != cell_entry => view.invalid.push((
                            key.to_string(),
                            "row key's entry segment is not the entry's content-derived id"
                                .to_string(),
                        )),
                        Ok((_, device)) if device != cell_author => view.invalid.push((
                            key.to_string(),
                            "row key's author segment is not the device the row's own carriage names"
                                .to_string(),
                        )),
                        Ok((member, device)) => {
                            let candidate = (enrolled_at_ms, device, member);
                            match enrolled.get(&cell_entry) {
                                Some((stamp, author, _))
                                    if (*stamp, *author) >= (candidate.0, candidate.1) => {}
                                _ => {
                                    enrolled.insert(cell_entry, candidate);
                                }
                            }
                        }
                        Err(reason) => view.invalid.push((key.to_string(), reason)),
                    }
                }
                GroupRosterRecord::Removed {
                    entry_id,
                    authorization,
                    removed_at_ms,
                    remover_sig,
                } => {
                    let verified = verify_removed_entry_parts(
                        &entry_id,
                        &authorization,
                        removed_at_ms,
                        &remover_sig,
                        authority,
                    );
                    match verified {
                        Ok(_) if entry_id != cell_entry => view.invalid.push((
                            key.to_string(),
                            "row key's entry segment is not the removal's entry id".to_string(),
                        )),
                        Ok(device) if device != cell_author => view.invalid.push((
                            key.to_string(),
                            "row key's author segment is not the device the row's own carriage names"
                                .to_string(),
                        )),
                        // Honored: the line does not refuse the remover, so
                        // the entry is excluded across every writer's cell.
                        Ok(_) => {
                            view.removed.insert(entry_id, ());
                        }
                        Err(reason) => view.invalid.push((key.to_string(), reason)),
                    }
                }
            }
        }
        view.members = enrolled
            .into_iter()
            .filter(|(entry_id, _)| !view.removed.contains_key(entry_id))
            .map(|(entry_id, (_, _, member))| (entry_id, member))
            .collect();
        // The whole view — the counted refusals included — is a function of
        // the merged SET of rows, never of the order they arrived in.
        view.invalid.sort();
        view
    }

    /// Is `entry_id` enrolled — some `Enrolled` cell verified under the line
    /// and no `Removed` cell did?
    pub fn is_enrolled_entry(&self, entry_id: &[u8; 32]) -> bool {
        self.members.contains_key(entry_id)
    }

    /// Does an **honored** `Removed` cell — one verifying under a device cert
    /// chaining to the authority root, its author's own standing not
    /// consulted — exclude `entry_id`? A stranger's removal never answers
    /// `true` here; a since-revoked authority device's still does.
    pub fn is_excluded_entry(&self, entry_id: &[u8; 32]) -> bool {
        self.removed.contains_key(entry_id)
    }

    /// The entry ids an honored `Removed` excludes, ascending — the
    /// severance pass's "did this scope ever sever" question, read as state.
    pub fn excluded_entries(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.removed.keys()
    }

    /// Is `actor` an enrolled member under **any** standing cell? The
    /// membership witness's question: a member removed under an old entry and
    /// re-admitted under a fresh one is a member again, and one removed under
    /// its only entry is not.
    pub fn is_verified_member(&self, actor: &ActorId) -> bool {
        self.wrap_targets().any(|m| m.member_actor == *actor)
    }

    /// The wrap targets a mint seals to: every enrolled member, in entry-id
    /// order.
    pub fn wrap_targets(&self) -> impl Iterator<Item = &RosterMember> {
        self.members.values()
    }

    /// Rows that verified as nothing: `(row key, reason)`.
    pub fn invalid(&self) -> &[(String, String)] {
        &self.invalid
    }
}

/// Verify one `Enrolled` roster record **on its own terms**, returning the
/// member it enrols. The load-bearing boundary, and deliberately the ONE
/// implementation behind both readers of a roster entry: the merged-plane
/// reader ([`RosterView::build`]) and the membership witness's verifier,
/// which evaluates an entry carried inline in the admission exchange
/// (`account-data-plane.md` § The admission seam — self-contained carriage).
/// Forking these two would let a witness be admitted by rules the plane
/// itself would refuse.
///
/// Mirrors [`crate::generation`]'s enrollment-cert check with one axis added:
/// the entry id is *recomputed from the core*, never taken on trust, so a
/// forged core cannot name a cell it did not earn.
///
/// # Errors
/// A core naming another scope, a core that does not encode, a malformed or
/// foreign-rooted authorization, an authorization expired before the
/// enrolment instant, an authorization covering a device the reader has
/// learned is revoked, an entry signature that is not the authorized
/// device's, or a binding signature that is absent or is not that device's.
pub fn verify_enrolled_entry(
    record: &GroupRosterRecord,
    scope_id: &[u8; 32],
    authority: &GroupAuthority,
) -> Result<RosterMember, String> {
    match record {
        GroupRosterRecord::Enrolled {
            core,
            reception_pubkey,
            authorization,
            authority_sig,
            enrolled_at_ms,
            binding_sig,
        } => verify_enrolled_entry_parts(
            core,
            reception_pubkey,
            authorization,
            authority_sig,
            *enrolled_at_ms,
            binding_sig,
            scope_id,
            authority,
        )
        .map(|(member, _)| member),
        GroupRosterRecord::Removed { .. } => Err("a Removed row enrols nobody".to_string()),
    }
}

/// Verify one `Removed` roster record **on its own terms** against the
/// authority chain, returning the authorized device that authored it: the
/// carriage chains to the authority root or a prior identity, is not expired
/// before the removal's asserted instant, and the remover signature verifies
/// under it over the entry id and the stamp. **The remover's own standing is
/// not consulted** ([`GroupAuthority::verify_device_chain`]): a removal is a
/// stop-trusting signal and survives its author's revocation, so revoking
/// the device that removed a member never resurrects the member. Only a
/// removal that passes this excludes anything ([`RosterView::build`]).
///
/// # Errors
/// A malformed, foreign-rooted or expired authorization, or a remover
/// signature that is not the authorized device's.
fn verify_removed_entry_parts(
    entry_id: &[u8; 32],
    authorization: &[u8],
    removed_at_ms: i64,
    remover_sig: &[u8],
    authority: &GroupAuthority,
) -> Result<[u8; 32], String> {
    let device_key =
        authority.verify_device_chain("removal authorization", authorization, removed_at_ms)?;
    if remover_sig.len() != 64
        || !crate::identity::verify_detached(
            &device_key,
            &roster_removal_signing_bytes(entry_id, removed_at_ms),
            remover_sig,
        )
    {
        return Err(
            "roster removal signature is absent or does not verify under the authorized device"
                .to_string(),
        );
    }
    Ok(device_key)
}

/// The shared `Enrolled` verification, returning the member beside the
/// authorized device that signed it (the view checks the latter against the
/// cell's author segment).
#[allow(clippy::too_many_arguments)]
fn verify_enrolled_entry_parts(
    core: &RosterEntryCore,
    reception_pubkey: &[u8],
    authorization: &[u8],
    authority_sig: &[u8],
    enrolled_at_ms: i64,
    binding_sig: &[u8],
    scope_id: &[u8; 32],
    authority: &GroupAuthority,
) -> Result<(RosterMember, [u8; 32]), String> {
    // The core must belong to the group being read — otherwise a valid entry
    // from another group replays into this roster.
    if core.scope_id != *scope_id {
        return Err("roster entry core names a different group scope".to_string());
    }
    let recomputed =
        roster_entry_id(core).map_err(|e| format!("roster entry core does not encode: {e}"))?;
    // Root-or-chain against the AUTHORITY line, expiry anchored to the
    // enrollment's asserted instant, and the device not learned revoked — the
    // one check every authority surface on the plane shares.
    let device_key =
        authority.verify_device_cert("authorization", authorization, enrolled_at_ms)?;
    // The entry itself must be signed by the authorized device.
    verify_roster_entry_sig(&device_key, &recomputed, authority_sig)?;
    // The binding must be present and verify under the same device. An entry
    // that CLAIMS the binding and fails it is a forgery under a carried-through
    // signature; an entry with NO binding is the unbound shape pre-ruling
    // binaries wrote, which no writer has produced since the ruling (every
    // production writer builds through `sign_roster_enrollment`) — so its only
    // producer left is the re-file forgery the binding exists to stop, and it
    // is refused outright (the compat-remnant sweep, 2026-09-25, program 4).
    // Refusing it here, not only outranking it at the join, closes the window
    // the join alone left: a replica adopting the forgery before the
    // authority's bound row reached it would have enrolled the attacker's key
    // as a wrap target until the merge.
    if binding_sig.len() != 64
        || !crate::identity::verify_detached(
            &device_key,
            &roster_binding_signing_bytes(&recomputed, reception_pubkey, enrolled_at_ms),
            binding_sig,
        )
    {
        return Err(
            "roster entry binding signature is absent or does not verify under the authorized device"
                .to_string(),
        );
    }
    Ok((
        RosterMember {
            entry_id: recomputed,
            member_actor: core.member_actor,
            reception_pubkey: reception_pubkey.to_vec(),
            enrolled_at_ms,
        },
        device_key,
    ))
}

fn verify_roster_entry_sig(
    device_key: &[u8; 32],
    entry_id: &[u8; 32],
    sig: &[u8],
) -> Result<(), String> {
    if sig.len() != 64 {
        return Err("roster entry signature must be 64 bytes".to_string());
    }
    if !crate::identity::verify_detached(device_key, &roster_entry_signing_bytes(entry_id), sig) {
        return Err(
            "roster entry signature does not verify under the authorized device".to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Capability, Timestamp};
    use crate::encoding::{canonical_encode, sign_envelope};
    use crate::identity::ActorKeypair;

    fn authority_keypair() -> ActorKeypair {
        ActorKeypair::from_secret([21u8; 32])
    }

    fn prior_keypair() -> ActorKeypair {
        ActorKeypair::from_secret([22u8; 32])
    }

    fn foreign_keypair() -> ActorKeypair {
        ActorKeypair::from_secret([23u8; 32])
    }

    fn member_actor(seed: u8) -> ActorId {
        ActorKeypair::from_secret([seed; 32]).actor_id()
    }

    fn device_signing_key(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    fn machinery_root() -> crate::crypto::GroupMachineryRoot {
        crate::crypto::GroupMachineryRoot::from_bytes([0xD7; 32])
    }

    fn birth() -> GroupBirthRecord {
        GroupBirthRecord {
            authority_actor: authority_keypair().actor_id(),
            salt: [0xB1; 32],
            machinery_root_commit: machinery_root().commitment(),
            created_at_ms: 1_700_000_000_000,
        }
    }

    fn scope() -> [u8; 32] {
        group_scope_id(&birth()).expect("scope id")
    }

    /// A birth record binds only the scope its content hashes to: filed under
    /// another scope's id — the forged-authority shape, the same root
    /// commitment naming someone else — it is refused, not decoded.
    #[test]
    fn a_birth_record_decodes_only_for_its_own_scope() {
        let bytes = canonical_encode(&birth()).unwrap();
        assert_eq!(decode_birth_for_scope(&bytes, &scope()).unwrap(), birth());

        let forged = GroupBirthRecord {
            authority_actor: foreign_keypair().actor_id(),
            ..birth()
        };
        let err = decode_birth_for_scope(&canonical_encode(&forged).unwrap(), &scope())
            .expect_err("a record hashing elsewhere is not this scope's");
        assert!(err.to_string().contains("not this scope's"), "{err}");
        assert!(decode_birth_for_scope(b"\xff", &scope()).is_err());
    }

    fn core(member_seed: u8, salt: u8) -> RosterEntryCore {
        RosterEntryCore {
            scope_id: scope(),
            member_actor: member_actor(member_seed),
            admission_salt: [salt; 32],
        }
    }

    /// A `DeviceAuthorization` for `device`, signed by `signer`, in the
    /// `EmbedAsBytes` carriage the roster row embeds.
    fn cert_bytes(signer: &ActorKeypair, device: [u8; 32], expires_at: Option<u64>) -> Vec<u8> {
        let cert = DeviceAuthorization {
            actor_id: signer.actor_id(),
            device_key: device,
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: expires_at.map(Timestamp),
        };
        let (bytes, env) = sign_envelope(signer, &cert).expect("sign cert");
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).expect("encode carriage")
    }

    /// A well-formed enrolled row, production's own shape (bound):
    /// `(logical key, value bytes)`.
    fn enrolled_row(
        signer: &ActorKeypair,
        device_seed: u8,
        core: RosterEntryCore,
    ) -> (String, Vec<u8>) {
        enrolled_row_at(signer, device_seed, core, 2_000)
    }

    /// [`enrolled_row`] at a chosen stamp.
    fn enrolled_row_at(
        signer: &ActorKeypair,
        device_seed: u8,
        core: RosterEntryCore,
        enrolled_at_ms: i64,
    ) -> (String, Vec<u8>) {
        let device = device_signing_key(device_seed);
        let (entry_id, record) = sign_roster_enrollment(
            &device,
            core,
            vec![0xE0; 8],
            cert_bytes(signer, device.verifying_key().to_bytes(), None),
            enrolled_at_ms,
        )
        .expect("entry");
        (
            roster_cell_key(&entry_id, &device.verifying_key().to_bytes()),
            canonical_encode(&record).expect("encode row"),
        )
    }

    /// The UNBOUND shape — what pre-ruling binaries wrote and only a forger
    /// produces now: refused at every reader since the compat-remnant sweep,
    /// still a lattice element (the join ranks it lowest, a removal absorbs
    /// it).
    fn unbound_enrolled_row(
        signer: &ActorKeypair,
        device_seed: u8,
        core: RosterEntryCore,
    ) -> (String, Vec<u8>) {
        let device = device_signing_key(device_seed);
        let entry_id = roster_entry_id(&core).expect("entry id");
        let value = canonical_encode(&GroupRosterRecord::Enrolled {
            core,
            reception_pubkey: vec![0xE0; 8],
            authorization: cert_bytes(signer, device.verifying_key().to_bytes(), None),
            authority_sig: sign_roster_entry(&device, &entry_id),
            enrolled_at_ms: 2_000,
            binding_sig: Vec::new(),
        })
        .expect("encode row");
        (
            roster_cell_key(&entry_id, &device.verifying_key().to_bytes()),
            value,
        )
    }

    /// An authored removal of `entry_id` by authority device `device_seed`
    /// under a cert signed by `signer`, production's own shape: `(logical
    /// key, value bytes)`.
    fn removed_row(
        signer: &ActorKeypair,
        device_seed: u8,
        entry_id: &[u8; 32],
    ) -> (String, Vec<u8>) {
        let device = device_signing_key(device_seed);
        let (key, record) = sign_roster_removal(
            &device,
            *entry_id,
            cert_bytes(signer, device.verifying_key().to_bytes(), None),
            3_000,
        );
        (key, canonical_encode(&record).expect("encode row"))
    }

    /// The authority line as a reader holding `revocations` sees it.
    fn authority_line(revocations: &[(String, Vec<u8>)]) -> GroupAuthority {
        GroupAuthority::build(
            &scope(),
            &authority_keypair().actor_id(),
            &[prior_keypair().actor_id()],
            revocations.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
        )
    }

    fn view(rows: &[(String, Vec<u8>)]) -> RosterView {
        view_under(&authority_line(&[]), rows)
    }

    fn view_under(authority: &GroupAuthority, rows: &[(String, Vec<u8>)]) -> RosterView {
        RosterView::build(
            &scope(),
            authority,
            rows.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
        )
    }

    /// A revocation of device `revoked_seed`, authored by authority device
    /// `revoker_seed` under a cert signed by `signer`.
    fn revocation_row(
        signer: &ActorKeypair,
        revoker_seed: u8,
        revoked_seed: u8,
    ) -> (String, Vec<u8>) {
        let revoker = device_signing_key(revoker_seed);
        let (key, record) = sign_authority_revocation(
            &revoker,
            cert_bytes(signer, revoker.verifying_key().to_bytes(), None),
            scope(),
            device_signing_key(revoked_seed).verifying_key().to_bytes(),
            5_000,
        );
        (key, canonical_encode(&record).expect("encode revocation"))
    }

    // ── scope identity ──────────────────────────────────────────────────────

    #[test]
    fn the_scope_id_is_a_deterministic_function_of_the_birth_record() {
        assert_eq!(
            group_scope_id(&birth()).unwrap(),
            group_scope_id(&birth()).unwrap()
        );
    }

    /// The id COMMITS to the authority actor: nobody can present a birth record
    /// naming an authority they do not hold and have it hash to an existing
    /// scope's id. Every field is load-bearing, so each perturbation moves it.
    #[test]
    fn every_birth_field_moves_the_scope_id() {
        let base = group_scope_id(&birth()).unwrap();

        let mut other_authority = birth();
        other_authority.authority_actor = foreign_keypair().actor_id();
        assert_ne!(group_scope_id(&other_authority).unwrap(), base);

        let mut other_salt = birth();
        other_salt.salt = [0xB2; 32];
        assert_ne!(group_scope_id(&other_salt).unwrap(), base);

        let mut other_stamp = birth();
        other_stamp.created_at_ms += 1;
        assert_ne!(group_scope_id(&other_stamp).unwrap(), base);

        let mut other_root = birth();
        other_root.machinery_root_commit =
            crate::crypto::GroupMachineryRoot::from_bytes([0xD8; 32]).commitment();
        assert_ne!(group_scope_id(&other_root).unwrap(), base);
    }

    /// The joiner-side half of the id↔root binding: the honest root verifies
    /// against the birth record its scope id was derived over, and a
    /// substituted root — even one whose commitment is well-formed — is a
    /// crisp refusal.
    #[test]
    fn a_substituted_machinery_root_is_refused_at_the_joiner() {
        assert!(verify_machinery_root_commitment(
            &birth(),
            &machinery_root()
        ));
        let substituted = crate::crypto::GroupMachineryRoot::from_bytes([0xD8; 32]);
        assert!(!verify_machinery_root_commitment(&birth(), &substituted));
    }

    #[test]
    fn the_entry_id_commits_to_scope_member_and_admission_salt() {
        let base = roster_entry_id(&core(0x31, 0x01)).unwrap();
        assert_ne!(roster_entry_id(&core(0x32, 0x01)).unwrap(), base, "member");
        assert_ne!(roster_entry_id(&core(0x31, 0x02)).unwrap(), base, "salt");
        let mut other_scope = core(0x31, 0x01);
        other_scope.scope_id = [0x00; 32];
        assert_ne!(roster_entry_id(&other_scope).unwrap(), base, "scope");
    }

    // ── the lattice ─────────────────────────────────────────────────────────

    /// The key-aware join at `cell`, owned bytes out.
    fn join_at(cell: &str, current: &[u8], incoming: &[u8]) -> Vec<u8> {
        let (entry, author) = parse_roster_cell_key(cell).expect("a roster cell key");
        join_group_roster(&entry, &author, current, incoming)
            .expect("both sides decode")
            .to_vec()
    }

    #[test]
    fn the_roster_join_is_commutative_and_idempotent() {
        let (cell, a) = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let (_, b) = enrolled_row(&authority_keypair(), 0x42, core(0x31, 0x01));
        assert_eq!(join_at(&cell, &a, &b), join_at(&cell, &b, &a));
        assert_eq!(join_at(&cell, &a, &a), a);
    }

    /// Decode-or-fail: bytes this build cannot read — junk, or a later
    /// build's record variant — are never ranked at a roster cell; the join
    /// fails in both orders, so the walk skips the row unaccounted and
    /// `reconcile` presents it again to a build that can read it.
    #[test]
    fn an_undecodable_side_fails_the_roster_join() {
        let (cell, honest) = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let (entry, author) = parse_roster_cell_key(&cell).expect("a roster cell key");
        for undecodable in [b"junk".to_vec(), crate::generation::tests::future_variant()] {
            assert!(join_group_roster(&entry, &author, &honest, &undecodable).is_err());
            assert!(join_group_roster(&entry, &author, &undecodable, &honest).is_err());
        }
    }

    /// **The per-writer cell at the join itself:** a
    /// self-verifying row from ANOTHER writer's cell — the shape a merge into
    /// a standing cell meets — never displaces the row standing there, in
    /// either order and whatever the bytes; at its own cell it stands. Found
    /// red at build: a key-free join let it win on bytes.
    #[test]
    fn a_row_from_another_writers_cell_never_displaces_the_standing_row() {
        let (mine, a) = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let (theirs, b) = enrolled_row(&authority_keypair(), 0x42, core(0x31, 0x01));
        assert_ne!(mine, theirs);
        assert_eq!(join_at(&mine, &a, &b), a);
        assert_eq!(join_at(&mine, &b, &a), a);
        assert_eq!(join_at(&theirs, &a, &b), b);
        assert_eq!(join_at(&theirs, &b, &a), b);
        // Neither row verifies at a third cell: bytes decide, both orders.
        let elsewhere = roster_cell_key(&[0x0E; 32], &[0x0F; 32]);
        let by_bytes = if a >= b { a.clone() } else { b.clone() };
        assert_eq!(join_at(&elsewhere, &a, &b), by_bytes);
        assert_eq!(join_at(&elsewhere, &b, &a), by_bytes);
    }

    /// Within one writer's cell, its own authored removal absorbs its own
    /// entry in either order — the two-phase lattice, per writer.
    #[test]
    fn a_writers_own_removal_absorbs_its_own_entry_in_either_order() {
        let c = core(0x31, 0x01);
        let entry_id = roster_entry_id(&c).unwrap();
        let (cell, enrolled) = enrolled_row(&authority_keypair(), 0x41, c);
        let (removed_cell, removed) = removed_row(&authority_keypair(), 0x41, &entry_id);
        assert_eq!(
            cell, removed_cell,
            "the removal lands in the writer's own cell"
        );
        assert_eq!(join_at(&cell, &enrolled, &removed), removed);
        assert_eq!(join_at(&cell, &removed, &enrolled), removed);
    }

    /// The join ranks self-verification above phase: a removal whose
    /// signature does not verify under its own carriage ranks below a bound
    /// entry, so a vandal's broken `Removed` filed into a cell never displaces
    /// the row there — and two verifying removals still converge by bytes.
    #[test]
    fn a_removal_that_does_not_self_verify_ranks_below_a_bound_entry() {
        let c = core(0x31, 0x01);
        let entry_id = roster_entry_id(&c).unwrap();
        let (cell, enrolled) = enrolled_row(&authority_keypair(), 0x41, c);
        let (_, removed) = removed_row(&authority_keypair(), 0x41, &entry_id);
        let GroupRosterRecord::Removed {
            entry_id: eid,
            authorization,
            removed_at_ms,
            ..
        } = canonical_decode::<GroupRosterRecord>(&removed).unwrap()
        else {
            unreachable!()
        };
        let broken = canonical_encode(&GroupRosterRecord::Removed {
            entry_id: eid,
            authorization,
            removed_at_ms,
            remover_sig: vec![0xFF; 64],
        })
        .unwrap();
        // Rank, not bytes, decides: the broken removal does not verify at
        // the cell, the bound entry does.
        let (entry, author) = parse_roster_cell_key(&cell).unwrap();
        assert!(
            !canonical_decode::<GroupRosterRecord>(&broken)
                .unwrap()
                .verifies_at(&entry, &author)
        );
        assert_eq!(join_at(&cell, &enrolled, &broken), enrolled);
        assert_eq!(join_at(&cell, &broken, &enrolled), enrolled);

        let (_, later) = {
            let device = device_signing_key(0x41);
            let (k, r) = sign_roster_removal(
                &device,
                entry_id,
                cert_bytes(
                    &authority_keypair(),
                    device.verifying_key().to_bytes(),
                    None,
                ),
                4_000,
            );
            (k, canonical_encode(&r).unwrap())
        };
        assert_eq!(
            join_at(&cell, &removed, &later),
            join_at(&cell, &later, &removed)
        );
    }

    /// The headline structural property, per writer and across writers: an
    /// honored removal excludes its ENTRY ID at every writer's cell (a live
    /// sibling's `Enrolled` at the same id enrols nobody once it stands), so
    /// re-admission of a removed member is a DIFFERENT entry id — "add-wins
    /// resurrection" is unrepresentable rather than merely refused.
    #[test]
    fn re_admission_is_a_fresh_entry_id_and_never_revives_the_removed_one() {
        let first = core(0x31, 0x01);
        let first_id = roster_entry_id(&first).unwrap();
        let (first_key, first_row) = enrolled_row(&authority_keypair(), 0x41, first.clone());
        // A live sibling's copy of the same entry, in its own cell.
        let (sibling_key, sibling_row) = enrolled_row(&authority_keypair(), 0x42, first);
        let (removed_key, removed) = removed_row(&authority_keypair(), 0x41, &first_id);
        assert_eq!(
            first_key, removed_key,
            "the removal lands in the remover's own cell"
        );
        assert_ne!(first_key, sibling_key, "the sibling's cell is its own");

        // Re-admitting the same member with a fresh salt is a new entry id.
        let second = core(0x31, 0x02);
        let second_id = roster_entry_id(&second).unwrap();
        let (second_key, second_row) = enrolled_row(&authority_keypair(), 0x41, second);
        assert_ne!(first_key, second_key);

        // The remover's own cell stays absorbed no matter what arrives at it...
        assert_eq!(join_at(&first_key, &removed, &first_row), removed);
        // ...the sibling's standing cell at the removed id enrols nobody...
        let v = view(&[
            (removed_key, removed),
            (sibling_key, sibling_row),
            (second_key, second_row),
        ]);
        assert!(v.is_excluded_entry(&first_id));
        assert!(!v.is_enrolled_entry(&first_id));
        // ...while the member is a member again, under the new entry id.
        assert!(v.is_enrolled_entry(&second_id));
        assert!(v.is_verified_member(&member_actor(0x31)));
        assert_eq!(v.wrap_targets().count(), 1);
        assert!(v.invalid().is_empty(), "{:?}", v.invalid());
    }

    /// **The per-writer cell's pin: a `Removed` that
    /// does not chain to the authority root excludes nobody.** Under the
    /// shared cell an unsigned `Removed` was honored unconditionally, so any
    /// machinery-root holder — every member ever admitted, of any account —
    /// severed any member with one row, for good. Now a removal under a
    /// foreign root, one under an expired cert, one with a broken signature
    /// and one filed into another device's cell are each flagged, and the
    /// member stands. (A removal by a device the line has since revoked is
    /// NOT in this set — `a_removal_survives_its_authors_revocation`.)
    #[test]
    fn a_non_authority_chained_removed_excludes_nobody() {
        let c = core(0x31, 0x01);
        let entry_id = roster_entry_id(&c).unwrap();
        let bound = enrolled_row(&authority_keypair(), 0x41, c);
        let foreign = removed_row(&foreign_keypair(), 0x77, &entry_id);
        let expired = {
            let device = device_signing_key(0x43);
            let (key, record) = sign_roster_removal(
                &device,
                entry_id,
                // Expires at second 1 = ms 1_000, before the 3_000 stamp.
                cert_bytes(
                    &authority_keypair(),
                    device.verifying_key().to_bytes(),
                    Some(1),
                ),
                3_000,
            );
            (key, canonical_encode(&record).unwrap())
        };
        let broken = {
            let (key, value) = removed_row(&authority_keypair(), 0x42, &entry_id);
            let GroupRosterRecord::Removed {
                entry_id,
                authorization,
                removed_at_ms,
                ..
            } = canonical_decode::<GroupRosterRecord>(&value).unwrap()
            else {
                unreachable!()
            };
            (
                key,
                canonical_encode(&GroupRosterRecord::Removed {
                    entry_id,
                    authorization,
                    removed_at_ms,
                    remover_sig: vec![0xFF; 64],
                })
                .unwrap(),
            )
        };
        // A genuine removal by 0x42, re-filed into 0x44's cell.
        let refiled = {
            let (_, value) = removed_row(&authority_keypair(), 0x42, &entry_id);
            (
                roster_cell_key(
                    &entry_id,
                    &device_signing_key(0x44).verifying_key().to_bytes(),
                ),
                value,
            )
        };
        let line = authority_line(&[]);
        let v = view_under(&line, &[bound, foreign, expired, broken, refiled]);
        assert!(!v.is_excluded_entry(&entry_id), "nobody's removal counted");
        assert!(v.is_enrolled_entry(&entry_id));
        assert!(v.is_verified_member(&member_actor(0x31)));
        assert_eq!(v.invalid().len(), 4, "{:?}", v.invalid());
        assert!(
            v.invalid().iter().any(|(_, r)| r.contains("expired")),
            "{:?}",
            v.invalid()
        );
        assert!(
            v.invalid()
                .iter()
                .any(|(_, r)| r.contains("author segment")),
            "{:?}",
            v.invalid()
        );

        // And the one removal the line honors — by a live device, in its own
        // cell — excludes the entry at every writer's cell.
        let honored = removed_row(&authority_keypair(), 0x42, &entry_id);
        let c2 = core(0x31, 0x01);
        let v = view_under(
            &line,
            &[enrolled_row(&authority_keypair(), 0x41, c2), honored],
        );
        assert!(v.is_excluded_entry(&entry_id));
        assert!(!v.is_enrolled_entry(&entry_id));
        assert!(v.wrap_targets().next().is_none());
    }

    /// **The re-squat, made unrepresentable: a
    /// revoked device's re-bind of a member's entry — same core, so the same
    /// entry id, bound to a key the thief holds, at a stamp that would have
    /// outranked the genuine row under the shared cell — lives in its own
    /// dead cell.** The live device's row stands, at the attested key; the
    /// thief's row is flagged and displaces nothing.
    #[test]
    fn a_revoked_devices_rebind_at_the_recorded_entry_displaces_nothing() {
        let c = core(0x31, 0x01);
        let entry_id = roster_entry_id(&c).unwrap();
        let genuine = enrolled_row(&authority_keypair(), 0x42, c.clone());
        let thief = device_signing_key(0x41);
        let (_, rebound) = sign_roster_enrollment(
            &thief,
            c,
            vec![0xFF; 8],
            cert_bytes(&authority_keypair(), thief.verifying_key().to_bytes(), None),
            9_000,
        )
        .unwrap();
        let rebound = (
            roster_cell_key(&entry_id, &thief.verifying_key().to_bytes()),
            canonical_encode(&rebound).unwrap(),
        );
        assert_ne!(genuine.0, rebound.0, "two cells, never one");
        let line = authority_line(&[revocation_row(&authority_keypair(), 0x42, 0x41)]);
        for rows in [
            vec![genuine.clone(), rebound.clone()],
            vec![rebound.clone(), genuine.clone()],
        ] {
            let v = view_under(&line, &rows);
            let targets: Vec<_> = v.wrap_targets().collect();
            assert_eq!(targets.len(), 1);
            assert_eq!(targets[0].entry_id, entry_id);
            assert_eq!(
                targets[0].reception_pubkey,
                vec![0xE0; 8],
                "the genuine key"
            );
            assert_eq!(v.invalid().len(), 1);
            assert!(v.invalid()[0].1.contains("revoked authority device"));
        }
    }

    /// **A removal survives its author's revocation.** A `Removed` is a "stop
    /// trusting" signal, the revocation kind's class: honored on its chain
    /// alone, the remover's own standing never consulted (rule (2) extended),
    /// so revoking the device that removed a member never resurrects the
    /// member — the live sibling's `Enrolled` at the same entry stays
    /// excluded, before and after the revocation merges, in either row
    /// order. The revoked device's OWN `Enrolled` stays refused (rule (3):
    /// positive signals die with their author).
    #[test]
    fn a_removal_survives_its_authors_revocation() {
        let c = core(0x31, 0x01);
        let entry_id = roster_entry_id(&c).unwrap();
        let by_live = enrolled_row(&authority_keypair(), 0x42, c.clone());
        let by_remover = enrolled_row(&authority_keypair(), 0x41, c);
        let removed = removed_row(&authority_keypair(), 0x41, &entry_id);
        let rows = vec![by_live, by_remover, removed];
        let before = view(&rows);
        assert!(before.is_excluded_entry(&entry_id));
        assert!(before.invalid().is_empty(), "{:?}", before.invalid());

        let line = authority_line(&[revocation_row(&authority_keypair(), 0x42, 0x41)]);
        let mut reversed = rows.clone();
        reversed.reverse();
        for rows in [rows, reversed] {
            let after = view_under(&line, &rows);
            assert!(
                after.is_excluded_entry(&entry_id),
                "the removal outlives its author's revocation"
            );
            assert!(!after.is_enrolled_entry(&entry_id));
            assert!(after.wrap_targets().next().is_none());
            assert_eq!(after.invalid().len(), 1, "{:?}", after.invalid());
            assert!(
                after.invalid()[0].1.contains("revoked authority device"),
                "only the revoked device's Enrolled is refused: {:?}",
                after.invalid()
            );
        }
    }

    /// Several honored `Enrolled` cells at one entry id enrol ONE member —
    /// the newest by stamp, ties by author — identically on every replica.
    #[test]
    fn concurrent_honored_cells_at_one_entry_enrol_the_newest_once() {
        let c = core(0x31, 0x01);
        let entry_id = roster_entry_id(&c).unwrap();
        let older = enrolled_row_at(&authority_keypair(), 0x41, c.clone(), 2_000);
        let newer = enrolled_row_at(&authority_keypair(), 0x42, c.clone(), 3_000);
        let tie = enrolled_row_at(&authority_keypair(), 0x43, c, 3_000);
        let forward = view(&[older.clone(), newer.clone(), tie.clone()]);
        let backward = view(&[tie, newer, older]);
        assert_eq!(forward, backward);
        let targets: Vec<_> = forward.wrap_targets().collect();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].entry_id, entry_id);
        assert_eq!(targets[0].enrolled_at_ms, 3_000);
        assert!(forward.invalid().is_empty());
    }

    #[test]
    fn roster_cell_keys_parse_canonical_or_nothing() {
        let entry = [0xA1; 32];
        let author = [0xB2; 32];
        let key = roster_cell_key(&entry, &author);
        assert_eq!(parse_roster_cell_key(&key), Some((entry, author)));
        assert!(parse_roster_cell_key(&crate::hex32::encode(&entry)).is_none());
        assert!(parse_roster_cell_key(&key.to_uppercase()).is_none());
        assert!(parse_roster_cell_key(&format!("{key}/extra")).is_none());
        assert!(parse_roster_cell_key("NOTHEX/NOTHEX").is_none());
    }

    // ── the verified view ───────────────────────────────────────────────────

    #[test]
    fn an_authority_signed_entry_is_a_wrap_target() {
        let c = core(0x31, 0x01);
        let entry_id = roster_entry_id(&c).unwrap();
        let v = view(&[enrolled_row(&authority_keypair(), 0x41, c)]);
        assert!(v.invalid().is_empty(), "unexpected: {:?}", v.invalid());
        assert!(v.is_enrolled_entry(&entry_id));
        assert!(v.is_verified_member(&member_actor(0x31)));
        let targets: Vec<_> = v.wrap_targets().collect();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].reception_pubkey, vec![0xE0; 8]);
    }

    /// The seam: a device authorized by SOME OTHER actor enrolls nobody here,
    /// even though its own cert is perfectly valid.
    #[test]
    fn a_chain_to_a_foreign_root_is_not_a_member() {
        let v = view(&[enrolled_row(&foreign_keypair(), 0x41, core(0x31, 0x01))]);
        assert!(v.wrap_targets().next().is_none());
        assert_eq!(v.invalid().len(), 1);
        assert!(
            v.invalid()[0]
                .1
                .contains("neither the authority root nor a prior")
        );
    }

    /// Succession-crossing: an entry signed under a prior identity keeps
    /// verifying, so the verdict flips only toward acceptance as `prior` grows.
    #[test]
    fn a_chain_to_a_prior_identity_still_verifies() {
        let v = view(&[enrolled_row(&prior_keypair(), 0x41, core(0x31, 0x01))]);
        assert!(v.invalid().is_empty(), "unexpected: {:?}", v.invalid());
        assert!(v.is_verified_member(&member_actor(0x31)));
    }

    /// A row squatting another cell's key verifies as nothing — the candidate
    /// discipline the mint resolver applies to generation ids.
    #[test]
    fn a_row_at_the_wrong_cell_verifies_as_nothing() {
        let (_, value) = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let entry_id = roster_entry_id(&core(0x31, 0x01)).unwrap();
        let author = device_signing_key(0x41).verifying_key().to_bytes();
        // Wrong entry segment.
        let v = view(&[(roster_cell_key(&[0xAA; 32], &author), value.clone())]);
        assert!(v.wrap_targets().next().is_none());
        assert!(v.invalid()[0].1.contains("content-derived id"));
        // Wrong author segment — another device's cell.
        let v = view(&[(roster_cell_key(&entry_id, &[0xAA; 32]), value.clone())]);
        assert!(v.wrap_targets().next().is_none());
        assert!(v.invalid()[0].1.contains("author segment"));
        // The retired one-segment key is no cell of this kind.
        let v = view(&[(crate::hex32::encode(&entry_id), value)]);
        assert!(v.wrap_targets().next().is_none());
        assert!(v.invalid()[0].1.contains("two canonical hex32 segments"));
    }

    /// A valid entry from ANOTHER group does not replay into this roster.
    #[test]
    fn an_entry_naming_another_scope_is_refused() {
        let mut c = core(0x31, 0x01);
        c.scope_id = [0x5A; 32];
        let v = view(&[enrolled_row(&authority_keypair(), 0x41, c)]);
        assert!(v.wrap_targets().next().is_none());
        assert!(v.invalid()[0].1.contains("different group scope"));
    }

    /// The authority cert names one device; another device's signature over
    /// the entry does not ride it.
    #[test]
    fn the_entry_signature_must_be_the_authorized_devices() {
        let authorized = device_signing_key(0x41);
        // Signed (entry and binding alike) by a DIFFERENT device than the
        // cert authorizes.
        let (entry_id, record) = sign_roster_enrollment(
            &device_signing_key(0x42),
            core(0x31, 0x01),
            vec![0xE0; 8],
            cert_bytes(
                &authority_keypair(),
                authorized.verifying_key().to_bytes(),
                None,
            ),
            2_000,
        )
        .unwrap();
        let value = canonical_encode(&record).unwrap();
        let v = view(&[(
            roster_cell_key(&entry_id, &authorized.verifying_key().to_bytes()),
            value,
        )]);
        assert!(v.wrap_targets().next().is_none());
        assert!(v.invalid()[0].1.contains("entry signature does not verify"));
    }

    #[test]
    fn an_expired_authorization_does_not_enroll() {
        let device = device_signing_key(0x41);
        let (entry_id, record) = sign_roster_enrollment(
            &device,
            core(0x31, 0x01),
            vec![0xE0; 8],
            // Expires at second 1 = ms 1_000, before the entry's 2_000 stamp.
            cert_bytes(
                &authority_keypair(),
                device.verifying_key().to_bytes(),
                Some(1),
            ),
            2_000,
        )
        .unwrap();
        let value = canonical_encode(&record).unwrap();
        let v = view(&[(
            roster_cell_key(&entry_id, &device.verifying_key().to_bytes()),
            value,
        )]);
        assert!(v.wrap_targets().next().is_none());
        assert!(v.invalid()[0].1.contains("expired"));
    }

    #[test]
    fn malformed_rows_are_counted_never_members() {
        let (good_key, good) = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let v = view(&[
            (good_key, good),
            ("NOTHEX".to_string(), vec![1, 2, 3]),
            (crate::hex32::encode(&[0xBB; 32]), vec![0xFF, 0xFF]),
        ]);
        assert_eq!(v.wrap_targets().count(), 1);
        assert_eq!(v.invalid().len(), 2);
    }

    /// **The pin at the join itself** (the device-set shape
    /// on the group plane). A group-plane key holder copies a member's
    /// authority-signed entry verbatim — core, cert, entry signature — and
    /// substitutes its OWN reception key, with bytes that win the byte-order
    /// max; every later group mint and top-up would then seal to the
    /// attacker under the member's name. The authority's binding signature
    /// over the key is what stops it: an entry whose binding verifies under
    /// its own embedded cert beats any bytes that do not, in both merge
    /// orders. Red-verified 2026-09-16: with the pre-binding byte-order join
    /// the unbound forgery won this assertion outright.
    #[test]
    fn a_bound_entry_beats_a_forged_reception_key_at_its_cell() {
        let (cell, honest) = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let (_, honest_unbound) =
            unbound_enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let GroupRosterRecord::Enrolled {
            core: c,
            authorization,
            authority_sig,
            enrolled_at_ms,
            binding_sig,
            ..
        } = canonical_decode::<GroupRosterRecord>(&honest).unwrap()
        else {
            unreachable!()
        };
        // The attacker's copy: everything verbatim but the wrap target,
        // whose bytes sort above the member's.
        let unbound_forgery = canonical_encode(&GroupRosterRecord::Enrolled {
            core: c.clone(),
            reception_pubkey: vec![0xFF; 8],
            authorization: authorization.clone(),
            authority_sig: authority_sig.clone(),
            enrolled_at_ms,
            binding_sig: Vec::new(),
        })
        .unwrap();
        assert!(
            unbound_forgery > honest_unbound,
            "the forgery is crafted to win byte order against the unbound entry"
        );
        // The additive posture, pinned: the bound entry's extra map key raises
        // its header byte, so byte order alone already keeps it over an
        // unbound forgery…
        assert!(honest > unbound_forgery);
        // …and stays open only to a forgery carrying the binding through
        // (a carried signature no longer covers the substituted key — refused
        // by every binary that ranks verification first, and by every reader).
        let carried_sig_forgery = canonical_encode(&GroupRosterRecord::Enrolled {
            core: c,
            reception_pubkey: vec![0xFF; 8],
            authorization,
            authority_sig,
            enrolled_at_ms,
            binding_sig,
        })
        .unwrap();
        for vandal in [unbound_forgery.as_slice(), carried_sig_forgery.as_slice()] {
            assert_eq!(
                join_at(&cell, &honest, vandal),
                honest,
                "a bound entry is never displaced by a re-keyed copy"
            );
            assert_eq!(
                join_at(&cell, vandal, &honest),
                honest,
                "…in either merge order"
            );
        }
        // Two unbound entries — neither enrols anyone at a reader — still
        // resolve by byte-order max: the lattice stays total over everything
        // a decoder accepts, so junk merges away instead of wedging a cell.
        let (_, a) = unbound_enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let (_, b) = unbound_enrolled_row(&authority_keypair(), 0x42, core(0x31, 0x01));
        let by_bytes = if a >= b { a.clone() } else { b.clone() };
        assert_eq!(join_at(&cell, &a, &b), by_bytes);
        assert_eq!(join_at(&cell, &b, &a), by_bytes);
        // And among self-verifying rows the phase decides: the writer's own
        // authored removal absorbs its own bound entry.
        let (_, r) = removed_row(
            &authority_keypair(),
            0x41,
            &roster_entry_id(&core(0x31, 0x01)).unwrap(),
        );
        assert_eq!(join_at(&cell, &honest, &r), r);
        assert_eq!(join_at(&cell, &r, &honest), r);
    }

    /// The binding covers every field it names and the entry id: a swapped
    /// key, a bumped stamp, a core moved under the same signature, or a
    /// binding signed by a device other than the cert's un-verifies it.
    #[test]
    fn the_binding_refuses_every_tampered_field() {
        let device = device_signing_key(0x41);
        let (_, record) = sign_roster_enrollment(
            &device,
            core(0x31, 0x01),
            vec![0xE0; 8],
            cert_bytes(
                &authority_keypair(),
                device.verifying_key().to_bytes(),
                None,
            ),
            2_000,
        )
        .unwrap();
        assert!(record.self_verifies());
        let GroupRosterRecord::Enrolled {
            core: c,
            reception_pubkey,
            authorization,
            authority_sig,
            enrolled_at_ms,
            binding_sig,
        } = record
        else {
            unreachable!()
        };
        let rebuilt = |c: RosterEntryCore, key: Vec<u8>, stamp: i64, sig: Vec<u8>| {
            GroupRosterRecord::Enrolled {
                core: c,
                reception_pubkey: key,
                authorization: authorization.clone(),
                authority_sig: authority_sig.clone(),
                enrolled_at_ms: stamp,
                binding_sig: sig,
            }
        };
        let mut moved_core = c.clone();
        moved_core.admission_salt = [0x02; 32];
        let tampered = [
            rebuilt(
                c.clone(),
                vec![0xFF; 8],
                enrolled_at_ms,
                binding_sig.clone(),
            ),
            rebuilt(
                c.clone(),
                reception_pubkey.clone(),
                enrolled_at_ms + 1,
                binding_sig.clone(),
            ),
            rebuilt(
                moved_core,
                reception_pubkey.clone(),
                enrolled_at_ms,
                binding_sig.clone(),
            ),
            rebuilt(
                c.clone(),
                reception_pubkey.clone(),
                enrolled_at_ms,
                vec![0xFF; 64],
            ),
            rebuilt(
                c.clone(),
                reception_pubkey.clone(),
                enrolled_at_ms,
                Vec::new(),
            ),
        ];
        for t in &tampered {
            assert!(!t.self_verifies(), "{t:?}");
        }
        // A binding by a device the cert does not name: self-consistency
        // fails at the join, and the reader refuses it too.
        let other = device_signing_key(0x42);
        let entry_id = roster_entry_id(&c).unwrap();
        let foreign_binding = {
            use ed25519_dalek::Signer;
            other
                .sign(&roster_binding_signing_bytes(
                    &entry_id,
                    &reception_pubkey,
                    enrolled_at_ms,
                ))
                .to_bytes()
                .to_vec()
        };
        let foreign = rebuilt(c, reception_pubkey, enrolled_at_ms, foreign_binding);
        assert!(!foreign.self_verifies());
        let v = view(&[(
            roster_cell_key(
                &entry_id,
                &device_signing_key(0x41).verifying_key().to_bytes(),
            ),
            canonical_encode(&foreign).unwrap(),
        )]);
        assert!(v.wrap_targets().next().is_none());
        assert!(v.invalid()[0].1.contains("binding signature"));
    }

    /// At the reader: a bound entry enrols with its own key; an entry that
    /// claims a binding and fails it is flagged, never a member, never
    /// demoted to the unbound shape; and the unbound shape itself — what
    /// pre-ruling binaries wrote, chain-verified as a member until the
    /// compat-remnant sweep retired its acceptance (2026-09-25, program 4) —
    /// is flagged too, never a member, never a wrap target: with no honest
    /// writer left producing it, an unbound row IS the re-file forgery the
    /// binding stops, whichever replica meets it first.
    #[test]
    fn the_reader_honours_bound_and_refuses_broken_and_unbound_alike() {
        let bound = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let unbound = unbound_enrolled_row(&authority_keypair(), 0x41, core(0x32, 0x01));
        let (broken_key, broken_value) = enrolled_row(&authority_keypair(), 0x41, core(0x33, 0x01));
        let GroupRosterRecord::Enrolled {
            core: broken_core,
            authorization,
            authority_sig,
            enrolled_at_ms,
            binding_sig,
            ..
        } = canonical_decode::<GroupRosterRecord>(&broken_value).unwrap()
        else {
            unreachable!()
        };
        let broken_record = GroupRosterRecord::Enrolled {
            core: broken_core,
            reception_pubkey: vec![0xFF; 8], // the attacker's key under a carried binding
            authorization,
            authority_sig,
            enrolled_at_ms,
            binding_sig,
        };
        let broken = (broken_key, canonical_encode(&broken_record).unwrap());
        let unbound_record: GroupRosterRecord = canonical_decode(&unbound.1).unwrap();
        let v = view(&[bound, unbound, broken]);
        let members: Vec<_> = v.wrap_targets().collect();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].member_actor, member_actor(0x31));
        assert_eq!(members[0].reception_pubkey, vec![0xE0; 8]);
        assert_eq!(v.invalid().len(), 2);
        assert!(
            v.invalid()
                .iter()
                .all(|(_, reason)| reason.contains("binding signature")),
            "{:?}",
            v.invalid()
        );
        // The admission witness door goes through the same verifier, so it
        // refuses the same entries — one implementation behind both readers.
        for (record, what) in [
            (&broken_record, "a broken binding"),
            (&unbound_record, "an absent binding"),
        ] {
            let refused = match verify_enrolled_entry(record, &scope(), &authority_line(&[])) {
                Ok(_) => panic!("{what} is refused as a carried witness too"),
                Err(reason) => reason,
            };
            assert!(refused.contains("binding signature"), "{what}: {refused}");
        }
    }

    // ── authority-device revocation ───────────────────────────────

    /// **The pin at the merged-roster reader.** Device 0x41 was removed from
    /// the authority's own account. It still holds its secret, its
    /// root-signed cert and the never-rotated machinery root, so it can still
    /// write a perfectly chain-valid entry — BOUND, since the binding is "the
    /// authority device's" and it still is one to a reader that never learned
    /// otherwise. A reader that HAS learned refuses both the entry and its
    /// binding, while a live sibling's entry stands.
    #[test]
    fn a_revoked_authority_devices_entry_and_binding_are_refused_at_the_view() {
        let by_revoked = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let by_live = enrolled_row(&authority_keypair(), 0x42, core(0x32, 0x01));
        let rows = [by_revoked, by_live];

        let unaware = view(&rows);
        assert_eq!(
            unaware.wrap_targets().count(),
            2,
            "the gap, before learning"
        );

        let line = authority_line(&[revocation_row(&authority_keypair(), 0x42, 0x41)]);
        assert!(
            line.invalid().is_empty(),
            "unexpected: {:?}",
            line.invalid()
        );
        let aware = view_under(&line, &rows);
        let members: Vec<_> = aware.wrap_targets().map(|m| m.member_actor).collect();
        assert_eq!(members, vec![member_actor(0x32)]);
        assert_eq!(aware.invalid().len(), 1);
        assert!(aware.invalid()[0].1.contains("revoked authority device"));
    }

    /// **The pin at the membership witness** — the direct peer-ingest door
    /// `devices.md`'s re-visit was owed for. The carried entry is
    /// self-contained and chain-valid; only the evaluator's own authority
    /// line can refuse it.
    #[test]
    fn a_revoked_authority_devices_entry_is_refused_as_a_carried_witness() {
        let (_, value) = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let record: GroupRosterRecord = canonical_decode(&value).unwrap();
        verify_enrolled_entry(&record, &scope(), &authority_line(&[]))
            .expect("chain-valid to a reader that has learned nothing");
        let line = authority_line(&[revocation_row(&authority_keypair(), 0x42, 0x41)]);
        let refused = verify_enrolled_entry(&record, &scope(), &line)
            .expect_err("a revoked device enrols nobody");
        assert!(refused.contains("revoked authority device"));
    }

    /// A revocation is TOTAL, never dated: the plane has no arbiter, so a
    /// revoked device backdating its stamp to before the revocation's buys
    /// nothing.
    #[test]
    fn a_backdated_entry_by_a_revoked_device_is_still_refused() {
        let row = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01)); // stamped 2_000
        let line = authority_line(&[revocation_row(&authority_keypair(), 0x42, 0x41)]); // 5_000
        assert!(view_under(&line, &[row]).wrap_targets().next().is_none());
    }

    /// AUTHORED, never unconditional: the machinery root is held by every
    /// member ever admitted, so a revocation by anyone but the authority line
    /// is counted and ignored — a former member cannot kill the authority.
    #[test]
    fn a_revocation_by_a_non_authority_key_revokes_nobody() {
        let foreign = revocation_row(&foreign_keypair(), 0x51, 0x41);
        let line = authority_line(&[foreign]);
        assert!(!line.is_revoked(&device_signing_key(0x41).verifying_key().to_bytes()));
        assert_eq!(line.invalid().len(), 1);
        assert!(
            line.invalid()[0]
                .1
                .contains("neither the authority root nor a prior")
        );
    }

    /// The root ceremony's voice on this plane: the authority actor key
    /// itself revokes, with no cert to carry.
    #[test]
    fn the_authority_root_itself_may_revoke() {
        let revoked = device_signing_key(0x41).verifying_key().to_bytes();
        let (key, record) = sign_authority_revocation(
            authority_keypair().signing_key(),
            Vec::new(),
            scope(),
            revoked,
            5_000,
        );
        let line = authority_line(&[(key, canonical_encode(&record).unwrap())]);
        assert!(
            line.invalid().is_empty(),
            "unexpected: {:?}",
            line.invalid()
        );
        assert!(line.is_revoked(&revoked));
    }

    /// The revoker's own standing is not consulted — a mutual-revocation race
    /// revokes BOTH on every replica, whatever order the rows arrived in
    /// (the `FleetView` ruling: fail-safe, and the only convergent answer).
    #[test]
    fn a_mutual_revocation_race_revokes_both_in_either_order() {
        let a = revocation_row(&authority_keypair(), 0x41, 0x42);
        let b = revocation_row(&authority_keypair(), 0x42, 0x41);
        let forward = authority_line(&[a.clone(), b.clone()]);
        let backward = authority_line(&[b, a]);
        assert_eq!(forward, backward);
        for seed in [0x41, 0x42] {
            assert!(forward.is_revoked(&device_signing_key(seed).verifying_key().to_bytes()));
        }
    }

    /// A revocation is scope-bound and cell-bound: re-filed under another
    /// device's cell, or replayed from another scope, it revokes nobody.
    #[test]
    fn a_refiled_or_foreign_scope_revocation_revokes_nobody() {
        let (_, value) = revocation_row(&authority_keypair(), 0x42, 0x41);
        let revoker = device_signing_key(0x42).verifying_key().to_bytes();
        let victim = device_signing_key(0x43).verifying_key().to_bytes();
        let refiled = (authority_revocation_cell_key(&victim, &revoker), value);
        let line = authority_line(&[refiled]);
        assert!(!line.is_revoked(&victim));
        assert!(line.invalid()[0].1.contains("does not verify at its cell"));

        let signer = device_signing_key(0x42);
        let (key, other_scope) = sign_authority_revocation(
            &signer,
            cert_bytes(&authority_keypair(), revoker, None),
            [0x5A; 32],
            victim,
            5_000,
        );
        let line = authority_line(&[(key, canonical_encode(&other_scope).unwrap())]);
        assert!(!line.is_revoked(&victim));
        assert!(line.invalid()[0].1.contains("different group scope"));
    }

    /// The join's laws, and its one preference: a row that verifies at the
    /// cell is never displaced by a record a vandal filed there that does not
    /// verify at it; bytes that do not decode fail the join (decode-or-fail).
    #[test]
    fn the_revocation_join_is_a_lattice_that_prefers_the_verifying_row() {
        let (key, honest) = revocation_row(&authority_keypair(), 0x42, 0x41);
        let (device, revoker) = parse_authority_revocation_cell_key(&key).unwrap();
        // Another revoker's genuine row, filed into this cell: it decodes,
        // and does not verify here.
        let (other_key, misfiled) = revocation_row(&authority_keypair(), 0x43, 0x41);
        assert_ne!(other_key, key);
        for (a, b) in [(&honest, &misfiled), (&misfiled, &honest)] {
            assert_eq!(
                join_authority_revocation(&device, &revoker, a, b).unwrap(),
                honest
            );
        }
        assert_eq!(
            join_authority_revocation(&device, &revoker, &honest, &honest).unwrap(),
            honest
        );
        let junk = vec![0xFF; honest.len() + 8]; // byte-order greater
        for undecodable in [junk, crate::generation::tests::future_variant()] {
            assert!(join_authority_revocation(&device, &revoker, &honest, &undecodable).is_err());
            assert!(join_authority_revocation(&device, &revoker, &undecodable, &honest).is_err());
        }
        assert!(parse_authority_revocation_cell_key("NOTHEX/NOTHEX").is_none());
        assert!(parse_authority_revocation_cell_key(&crate::hex32::encode(&device)).is_none());
    }

    #[test]
    fn roster_records_round_trip_canonically() {
        let (_, bound) = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let (_, unbound) = unbound_enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        for bytes in [bound, unbound.clone()] {
            let rec: GroupRosterRecord = canonical_decode(&bytes).unwrap();
            assert_eq!(canonical_encode(&rec).unwrap(), bytes);
        }
        // The additive field discipline: an unbound row decodes (and
        // re-encodes canonically, without the key) — the verifier, not the
        // decoder, is what refuses it.
        assert!(
            !unbound.windows(11).any(|w| w == b"binding_sig"),
            "an unbound entry carries no binding key at all"
        );
    }

    /// Build is a pure function of merged rows: row order never changes the
    /// view, which is what lets mint admissibility be roster coverage with no
    /// arbiter anywhere.
    #[test]
    fn the_view_is_independent_of_row_order() {
        let a = enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01));
        let b = enrolled_row(&authority_keypair(), 0x41, core(0x32, 0x01));
        let forward = view(&[a.clone(), b.clone()]);
        let backward = view(&[b, a]);
        assert_eq!(forward, backward);
    }

    /// The fold is a pure function of the merged cells and the line: two replicas merging the same rows — several
    /// writers' cells per entry, honored and dead removals among them — in
    /// opposite orders fold alike, and the `Removed` never has to "arrive
    /// after" the row it excludes.
    #[test]
    fn two_replicas_merging_the_same_cells_in_opposite_orders_fold_alike() {
        let e31 = roster_entry_id(&core(0x31, 0x01)).unwrap();
        let e32 = roster_entry_id(&core(0x32, 0x01)).unwrap();
        let e33 = roster_entry_id(&core(0x33, 0x01)).unwrap();
        let rows = vec![
            enrolled_row(&authority_keypair(), 0x41, core(0x31, 0x01)),
            enrolled_row(&authority_keypair(), 0x42, core(0x31, 0x01)),
            removed_row(&authority_keypair(), 0x42, &e31),
            enrolled_row(&authority_keypair(), 0x41, core(0x32, 0x01)),
            enrolled_row(&authority_keypair(), 0x43, core(0x32, 0x01)), // dead: 0x43 revoked
            removed_row(&authority_keypair(), 0x43, &e32), // honored: a removal outlives it
            enrolled_row(&authority_keypair(), 0x41, core(0x33, 0x01)),
            removed_row(&foreign_keypair(), 0x77, &e33), // dead: foreign root
        ];
        let line = authority_line(&[revocation_row(&authority_keypair(), 0x41, 0x43)]);
        let forward = view_under(&line, &rows);
        let mut reversed = rows.clone();
        reversed.reverse();
        let backward = view_under(&line, &reversed);
        assert_eq!(forward, backward);
        assert!(forward.is_excluded_entry(&e31));
        assert!(!forward.is_enrolled_entry(&e31));
        assert!(forward.is_excluded_entry(&e32));
        assert!(!forward.is_enrolled_entry(&e32));
        assert!(!forward.is_excluded_entry(&e33));
        assert!(forward.is_enrolled_entry(&e33));
        assert_eq!(forward.wrap_targets().count(), 1);
        assert_eq!(forward.invalid().len(), 2, "{:?}", forward.invalid());
    }
}
