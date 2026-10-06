// MLS engine — manages groups, key packages, and message encryption.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use openmls::framing::errors::{MessageDecryptionError, SecretTreeError};
use openmls::group::StageCommitError;
use openmls::prelude::tls_codec::Deserialize as TlsDeserializeTrait;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::types::SignatureScheme;

use fauna_core::identity::{ActorId, ActorKeypair};

use crate::error::{MlsError, Result};
use crate::room_policy::{
    CommitVerdict, ROOM_POLICY_EXTENSION_TYPE, RoomPolicyExtension, RoomRole, StagedCommitFacts,
    judge_commit,
};
#[cfg(feature = "native")]
use crate::storage::SqliteStorage;
use crate::types::{
    ChannelEnvelope, ChannelId, ChannelMessage, ChannelMessageBody, SchedulingDelivery,
};
use fauna_core::data::Timestamp;

/// The ciphersuite used across all Fauna MLS operations.
const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519;

/// The RFC 9420 § 8.5 exporter label an end-to-end room's **room-post** base
/// secret derives under ([`MlsEngine::export_room_post_secret`]; `ui/feed.md`
/// § Encryption at rest → *Room-restricted*). Frozen: every room-restricted
/// post an end-to-end room member ever sealed opens through this string, so
/// editing it would lock every one of them for every member.
const ROOM_POST_EXPORT_LABEL: &str = "fauna.room.post.v1";

/// Provider-KV key prefix under which the engine records the **commit identity**
/// — `blake3` of the wire commit bytes — of each channel's staged, unmerged
/// pending commit ([`MlsEngine::pending_commit_hash`]).
///
/// **Why the provider KV and not a plain engine field.** A pending commit is
/// only useful to a *restart* path, and a restart restores exactly one thing:
/// the provider KV (native [`MlsEngine::save_state`] → SQLite snapshot; wasm and
/// cross-device → the `provider` replica blob). Storing the identity anywhere
/// else would let `{pending, its identity}` tear apart across a crash — the
/// pending would survive without the identity that proves *which* commit it is,
/// which is precisely the state that lets a resume merge a foreign
/// commit. In the KV they
/// are one blob under one CAS, so the torn pair is unrepresentable — the same
/// discipline `devices.md` § Cross-device MLS group-state sync Rule 2 applies to
/// the ingest cursor.
///
/// **Additive in both directions, no migration.** The snapshot encoding is an
/// opaque key-value map ([`encode_provider_snapshot`]), so a snapshot with no
/// pending commit carries no hash key (read back as `None`: callers refuse a
/// mismatch or never merge) and a binary ignores a key it never looks up.
/// openMLS only ever `get`s by an exact computed key, never enumerates the KV,
/// so a foreign key is inert to it.
const PENDING_COMMIT_HASH_PREFIX: &[u8] = b"fauna:pending_commit_hash:";

/// The provider-KV key holding `channel_id`'s pending-commit identity.
fn pending_commit_hash_key(channel_id: &ChannelId) -> Vec<u8> {
    let mut key = Vec::with_capacity(PENDING_COMMIT_HASH_PREFIX.len() + 32);
    key.extend_from_slice(PENDING_COMMIT_HASH_PREFIX);
    key.extend_from_slice(&channel_id.0);
    key
}

/// The native SQLite provider snapshot's at-rest shape: the KV as key-sorted
/// `(key, value)` pairs, each a CBOR byte string — the shape the cross-device
/// `provider` replica already carries ([`crate::state_replica::ProviderReplica`]),
/// and the one shape every raw-byte field takes
/// (`docs/goal/architecture/serialization.md` § Canonical IPLD dag-cbor,
/// "Variable-length byte fields"). Sorting makes equal state encode
/// byte-identically.
#[cfg_attr(not(feature = "native"), allow(dead_code))]
fn encode_provider_snapshot(values: &HashMap<Vec<u8>, Vec<u8>>) -> Result<Vec<u8>> {
    let mut pairs: Vec<(&serde_bytes::Bytes, &serde_bytes::Bytes)> = values
        .iter()
        .map(|(k, v)| (serde_bytes::Bytes::new(k), serde_bytes::Bytes::new(v)))
        .collect();
    pairs.sort_unstable_by(|a, b| a.0.cmp(b.0));
    fauna_cbor::encode_canonical(&pairs).map_err(|e| MlsError::Encoding(e.to_string()))
}

/// Read back [`encode_provider_snapshot`]'s bytes.
#[cfg_attr(not(feature = "native"), allow(dead_code))]
fn decode_provider_snapshot(
    bytes: &[u8],
) -> std::result::Result<HashMap<Vec<u8>, Vec<u8>>, fauna_cbor::DecodeError> {
    let pairs: Vec<(serde_bytes::ByteBuf, serde_bytes::ByteBuf)> =
        fauna_cbor::decode_strict(bytes)?;
    Ok(pairs
        .into_iter()
        .map(|(k, v)| (k.into_vec(), v.into_vec()))
        .collect())
}

/// The prefix every one of fauna's own provider-KV keys shares. Each is a
/// **per-channel marker** of the shape `fauna:<name>:<32-byte channel id>` —
/// every builder below appends the raw `ChannelId` last — which is what lets
/// the launch swap carry a local-only group's markers beside the entries
/// openMLS attributes to it ([`is_fauna_channel_key`]).
const FAUNA_KEY_PREFIX: &[u8] = b"fauna:";

/// One local-only group the provider swap carries across
/// ([`MlsEngine::restore_from_provider_storage`]): its channel, its raw MLS
/// group id, and the entries to re-insert over the swapped-in snapshot.
struct CarriedGroup {
    channel_id: ChannelId,
    raw_group_id: Vec<u8>,
    entries: Vec<(Vec<u8>, Vec<u8>)>,
}

/// Whether `key` is one of fauna's own per-channel markers for `channel_id`.
fn is_fauna_channel_key(key: &[u8], channel_id: &ChannelId) -> bool {
    key.len() > FAUNA_KEY_PREFIX.len() + 32
        && key.starts_with(FAUNA_KEY_PREFIX)
        && key.ends_with(&channel_id.0)
}

/// Provider-KV key prefix under which the engine memoizes, per channel, the
/// identity of an inbound commit whose bytes **may have consumed local ratchet
/// state without merging** — armed *across* `process_commit`'s consumption
/// window (before `process_message` begins its decrypt), kept on a
/// post-consumption local failure (a stage-local or merge failure), dropped on
/// success and on every intrinsically-invalid verdict. Value layout: 32-byte
/// blake3 of the wire commit bytes ++ 8-byte big-endian group epoch it failed
/// at ([`MlsEngine::merge_failed_commit_memo`]).
///
/// **Load-bearing for the retry** (Rule 2, `devices.md` § Cross-device MLS
/// group-state sync): PrivateMessage decryption consumes the sender-ratchet
/// generation the moment the decrypt runs — *before* any staging or merge
/// verdict exists — so reprocessing the same bytes over the consumed provider
/// can never succeed: the retry would surface as a decrypt validation failure
/// and be misclassified as [`MlsError::InvalidCommit`] (cursor advances → the
/// Rule-2 silent-loss this memo exists to prevent). With the memo, the retry
/// keeps reporting the same *local* failure until a replica resync (or a
/// relaunch onto a pre-consumption snapshot) heals the channel; an entry whose
/// epoch the group has since advanced past is stale and dropped.
///
/// **Why the provider KV and not an engine field** (the
/// [`PENDING_COMMIT_HASH_PREFIX`] discipline, and the fix for the review's
/// "relaunch restores the pre-consumption snapshot" refutation): the consumed
/// generation lives in the provider KV, so its memo must live in the *same
/// blob under the same CAS* — a snapshot persisted after the consumption
/// (`save_state` runs on every group create/join, gate-less add/remove,
/// resumed-pending clear, own-leaf merge, …) then carries the memo *with* it,
/// and a snapshot from before carries neither. A torn
/// `{consumed-provider, no-memo}` pair — the shape that re-opens the loss one
/// pass after relaunch — is unrepresentable. Same additive compat argument as
/// the pending-commit hash: old snapshots simply lack the key, old binaries
/// carry it inertly.
const MERGE_FAILED_COMMIT_PREFIX: &[u8] = b"fauna:merge_failed_commit:";

/// The provider-KV key holding `channel_id`'s merge-failed-commit memo.
fn merge_failed_commit_key(channel_id: &ChannelId) -> Vec<u8> {
    let mut key = Vec::with_capacity(MERGE_FAILED_COMMIT_PREFIX.len() + 32);
    key.extend_from_slice(MERGE_FAILED_COMMIT_PREFIX);
    key.extend_from_slice(&channel_id.0);
    key
}

/// Provider-KV key prefix marking a channel as a **chat** channel — stamped by
/// `FaunaMlsBackend::bind_channel` the moment a thread is bound
/// ([`MlsEngine::mark_channel_chat`]). Value: `b"1"` (presence is the signal).
///
/// **Why durable, and why the provider KV**: the background folder commit sweep derives its channel set
/// from the engine's groups minus the *RAM-only* thread bindings and
/// scheduling set, so after a relaunch an **unbound chat channel** was
/// indistinguishable from a folder channel — and the sweep applies
/// membership commits while skipping application messages, so it could
/// advance the shared engine's epoch past an unread chat message, which MLS
/// forward secrecy then makes permanently undecryptable (silent chat loss).
/// In the provider KV the marker rides the *same* snapshot/replica blob that
/// delivers the group itself — a group restored from a replica arrives
/// **with** its chat marker, atomically, so the unbound-relaunch window
/// cannot misclassify it. Never cleared: a chat channel never becomes a
/// folder channel, and a marker for a forgotten group is inert (every
/// consumer also requires `has_group`). Same additive at-rest compat argument
/// as [`PENDING_COMMIT_HASH_PREFIX`]: a channel without the key simply lacks it
/// (a folder channel, or a chat group before its bind stamps it, keeps today's
/// derivation), old binaries carry it inertly.
const CHANNEL_KIND_CHAT_PREFIX: &[u8] = b"fauna:channel_kind_chat:";

/// The provider-KV key holding `channel_id`'s chat marker. `pub(crate)` so a
/// [`crate::state_replica::ProviderReplica`] can answer the same question of
/// its own bytes, without loading them into an engine.
pub(crate) fn channel_kind_chat_key(channel_id: &ChannelId) -> Vec<u8> {
    let mut key = Vec::with_capacity(CHANNEL_KIND_CHAT_PREFIX.len() + 32);
    key.extend_from_slice(CHANNEL_KIND_CHAT_PREFIX);
    key.extend_from_slice(&channel_id.0);
    key
}

/// The scheduling twin of [`CHANNEL_KIND_CHAT_PREFIX`] — a **durable** marker
/// that `channel_id` is a one-off scheduling (iMIP) delivery. The in-memory
/// scheduling set in `fauna-conversations` empties on relaunch while the engine
/// persists its groups, so without this a relaunched (or organizer-side, which
/// never marked at all) scheduling channel classified as folder rail. For the
/// folder sweep that over-approximation was declared harmless (a scheduling
/// log carries no membership commits); for the cross-group eviction it is not —
/// a scheduling seat misclassified as folder would block a review verdict
/// with a remedy that does not exist (`identity-succession.md` § Propagation
/// rule (5)). Stamped at both creation sites ([`MlsEngine::build_scheduling_delivery`]
/// covers every organizer, the conversations backend's welcome-ingest covers
/// the recipient); never cleared; same additive at-rest compat as the chat
/// marker — a scheduling channel without the key keeps the old
/// derivation.
const CHANNEL_KIND_SCHEDULING_PREFIX: &[u8] = b"fauna:channel_kind_scheduling:";

/// The provider-KV key holding `channel_id`'s scheduling marker.
fn channel_kind_scheduling_key(channel_id: &ChannelId) -> Vec<u8> {
    let mut key = Vec::with_capacity(CHANNEL_KIND_SCHEDULING_PREFIX.len() + 32);
    key.extend_from_slice(CHANNEL_KIND_SCHEDULING_PREFIX);
    key.extend_from_slice(&channel_id.0);
    key
}

/// Provider-KV key prefix holding a **folder channel's owner** (the 32-byte
/// ActorId of the set's sharer — the nest-side channel claimant). Presence is
/// also the *positive* folder marker the commit policy keys on: unlike the
/// folder sweep's "neither chat nor scheduling" derivation, the policy must
/// never fire on a misclassified chat channel (it would refuse a group
/// chat's ordinary member commits), so it fires only on channels explicitly
/// stamped here. Stamped owner-side at folder-group mint
/// (`fauna-client-folders`' group seam) and member-side at
/// `join_folder_welcome` from the Welcome's MLS-authenticated sender (the
/// owner, by the owner-only-Adds invariant this same policy preserves —
/// `federation.md` § Cross-nest shared folders + channel append). Never
/// cleared; a channel without the key keeps today's open-commit
/// processing; old binaries carry it inertly (the standard additive at-rest
/// compat argument).
const FOLDER_CHANNEL_OWNER_PREFIX: &[u8] = b"fauna:folder_channel_owner:";

/// The provider-KV key holding `channel_id`'s folder-owner marker.
fn folder_channel_owner_key(channel_id: &ChannelId) -> Vec<u8> {
    let mut key = Vec::with_capacity(FOLDER_CHANNEL_OWNER_PREFIX.len() + 32);
    key.extend_from_slice(FOLDER_CHANNEL_OWNER_PREFIX);
    key.extend_from_slice(&channel_id.0);
    key
}

/// Provider-KV key prefix recording the **Welcome sender** (32-byte ActorId of
/// the member whose commit produced the Welcome this device joined from) for
/// every joined channel. Recorded at join, after the MLS-2 whole-roster
/// credential validation, so the identity is leaf-verified. Consumers: the
/// folder join stamps [`FOLDER_CHANNEL_OWNER_PREFIX`] from it (a folder
/// Welcome's sender is the owner). Additive at-rest: a channel not joined from a Welcome lacks the
/// key; old binaries carry it inertly.
const WELCOME_SENDER_PREFIX: &[u8] = b"fauna:welcome_sender:";

/// The provider-KV key holding `channel_id`'s welcome-sender record.
fn welcome_sender_key(channel_id: &ChannelId) -> Vec<u8> {
    let mut key = Vec::with_capacity(WELCOME_SENDER_PREFIX.len() + 32);
    key.extend_from_slice(WELCOME_SENDER_PREFIX);
    key.extend_from_slice(&channel_id.0);
    key
}

/// Provider-KV key prefix recording this device's **history admission** for a
/// channel it joined from a Welcome: the epoch it joined at (8 bytes,
/// big-endian), written at join beside [`WELCOME_SENDER_PREFIX`] and REMOVED
/// when the admission is spent ([`MlsEngine::take_history_admission`]). The
/// record existing is the statement "this device is a newcomer still owed its
/// one history slice" (`conversation-rooms.md` § History for joiners → *What a
/// device accepts*). Additive at-rest: a channel this device founded, or whose
/// admission is spent, has none and is owed nothing.
const HISTORY_ADMISSION_PREFIX: &[u8] = b"fauna:history_admission:";

/// The provider-KV key holding `channel_id`'s history-admission record.
fn history_admission_key(channel_id: &ChannelId) -> Vec<u8> {
    let mut key = Vec::with_capacity(HISTORY_ADMISSION_PREFIX.len() + 32);
    key.extend_from_slice(HISTORY_ADMISSION_PREFIX);
    key.extend_from_slice(&channel_id.0);
    key
}

/// Provider-KV key prefix for the **policy-refused-commit memo**: the blake3
/// identities (+ the group epoch they were refused at) of commits
/// [`MlsError::PolicyRefusedCommit`] rejected on this channel. Needed because
/// the refusal verdict is reached *after* PrivateMessage decryption consumed
/// the committer's sender-ratchet generation: the folder rail re-walks the
/// channel log from seq 0 every launch, and re-decrypting the same bytes
/// yields `SecretTreeError::SecretReuseError` — a *local-stall* verdict (see
/// `classify_decrypt_error`), so without this memo one hostile commit would
/// wedge the member's folder rail on every subsequent launch. The memo is
/// checked before decryption and answers the replay with the same typed
/// refusal. Entries at epochs the group has advanced past are pruned on
/// touch — openMLS's epoch precheck already skips those pre-decrypt as
/// `PastEpochCommit` — so the set only ever holds current-epoch refusals
/// (capped as a spam backstop; an in-group flooder past the cap degrades to
/// the pre-existing loud-stall + replica-resync posture).
/// Value: concatenated 40-byte entries (32-byte hash ‖ 8-byte BE epoch).
/// Additive at-rest, like every key above.
const POLICY_REFUSED_COMMIT_PREFIX: &[u8] = b"fauna:policy_refused_commit:";

/// The most current-epoch refusals the memo retains per channel (spam backstop).
const POLICY_REFUSED_COMMIT_CAP: usize = 512;

/// The provider-KV key holding `channel_id`'s policy-refused-commit memo.
fn policy_refused_commit_key(channel_id: &ChannelId) -> Vec<u8> {
    let mut key = Vec::with_capacity(POLICY_REFUSED_COMMIT_PREFIX.len() + 32);
    key.extend_from_slice(POLICY_REFUSED_COMMIT_PREFIX);
    key.extend_from_slice(&channel_id.0);
    key
}

/// Provider-KV key prefix for the **at-rest folder park**: the canonical
/// `SignedIdentitySuccession` bytes of a succession statement this seat's
/// witness refused on a folder channel whose recorded owner
/// ([`FOLDER_CHANNEL_OWNER_PREFIX`]) it names — mirrored here by the
/// conversations backend so the folder commit walk's hold behind it survives
/// the session (`federation.md` § Cross-nest shared folders + channel append →
/// *The folder commit walk inherits the harvest wait*). One slot per channel,
/// latest wins; forgotten when the harvest speaks for the owner and with the
/// group ([`MlsEngine::forget_group`]). Derived, recreatable state — the
/// statement rides the channel log whether or not this seat consumed it, and
/// losing the slot only reopens the pre-park residual. Additive at-rest, like
/// every key above.
const PARKED_FOLDER_SUCCESSION_PREFIX: &[u8] = b"fauna:parked_folder_succession:";

/// The provider-KV key holding `channel_id`'s at-rest folder park.
fn parked_folder_succession_key(channel_id: &ChannelId) -> Vec<u8> {
    let mut key = Vec::with_capacity(PARKED_FOLDER_SUCCESSION_PREFIX.len() + 32);
    key.extend_from_slice(PARKED_FOLDER_SUCCESSION_PREFIX);
    key.extend_from_slice(&channel_id.0);
    key
}

/// Result of decrypting with ordering checks.
pub enum OrderedDecryptResult {
    /// Message is in order, deliver it.
    Deliver(ChannelMessage),
    /// Message has a sequence gap (missed messages), deliver but flag.
    DeliverWithGap {
        message: ChannelMessage,
        expected_seq: u64,
        actual_seq: u64,
    },
    /// Duplicate message, discard.
    Duplicate,
}

/// An application message as [`MlsEngine::decrypt_authenticated`] opened it:
/// the payload, with `sender` overwritten by the MLS-authenticated one, plus
/// the two facts about it that are readable only at the decrypt.
#[derive(Debug, Clone)]
pub struct DecryptedMessage {
    pub message: ChannelMessage,
    /// The sender's role under the policy of the epoch the message was sealed
    /// in; `None` is "no role to vouch for" and fails closed.
    pub sender_role: Option<RoomRole>,
    /// The epoch the message was sealed in, off the MLS framing
    /// `process_message` authenticates — never the payload's self-asserted
    /// `channel_epoch`.
    pub epoch: u64,
}

/// Core MLS engine that manages group state, key packages, and message
/// encryption/decryption.
///
/// Each engine is bound to a single actor identity and uses an in-memory
/// OpenMLS provider with a SQLite-backed persistence layer for long-lived
/// state.
pub struct MlsEngine {
    provider: OpenMlsRustCrypto,
    identity: ActorKeypair,
    credential: CredentialWithKey,
    signer: SignatureKeyPair,
    #[cfg(feature = "native")]
    pub(crate) storage: SqliteStorage,
    groups: Mutex<HashMap<ChannelId, MlsGroup>>,
    // The merge-failed-commit memo lives in the provider KV, not here — see
    // `MERGE_FAILED_COMMIT_PREFIX`: it must ride the same blob/CAS as the
    // consumed ratchet state it annotates.
    /// Channels whose staged pending commit was **reloaded from the provider
    /// snapshot** rather than staged by this process — the gate-less
    /// crash-reconcile discriminator (`devices.md` § Durability rules Rule 1,
    /// the durable-local-pending heal). A *resumed* pending's send died with
    /// the previous process, so once a full log walk finds no matching own
    /// commit it is provably undistributed and safe to clear; a *live* pending
    /// must never be cleared from the poll path (its send may still be in
    /// flight). Populated by the restoring constructor, emptied per channel by
    /// any in-process stage / merge / clear.
    resumed_pending_channels: Mutex<HashSet<ChannelId>>,
    /// This device's **last-seen replica listing**: the channels named by a
    /// replica listing this engine adopted (the provider swap, the targeted
    /// import) or authored (a flush of its own export that landed), less the
    /// ones it has since forgotten. The ancestor
    /// [`Self::restore_from_provider_storage`] asks its join-or-deletion
    /// question against: a local-only group this names was deleted from the
    /// listing by another device, and one it does not name was joined since
    /// (`devices.md` § Cross-device MLS group-state sync → *A sibling's
    /// deletion is not a join*). Durable on native, because the launch door runs
    /// in a fresh process. Writes go through to storage, so the in-memory set is
    /// the read side on every target.
    replica_listed: Mutex<HashSet<ChannelId>>,
    /// Set by [`MlsEngine::retire`] once the conversations-engine role has been
    /// handed to a successor: every group-touching method then refuses
    /// [`MlsError::Retired`].
    ///
    /// **Why this is not the storage poison.** `SqliteStorage::retire` refuses
    /// every *database* statement, and that is complete — but the hazard its own
    /// doc names is two engines advancing one group's epochs, and epochs do not
    /// live in SQLite. They live in the in-memory openMLS provider, which
    /// `encrypt` and `decrypt` mutate without touching storage at all. So the
    /// poison alone left a retired engine able to seal valid ciphertext under
    /// the account's own leaf and to originate Commits, persisting none of it
    /// (every `save_state` failure on those paths is a swallowed warn) — a live
    /// sealer whose writes are *guaranteed* to fail, which is the ratchet-fork
    /// condition itself.
    retired: AtomicBool,
}

/// Enforce the Fauna **leaf-credential binding** (security review MLS-2,
/// 2026-06-27): an MLS leaf's `BasicCredential` content MUST equal its leaf
/// signature key.
///
/// In Fauna an `ActorId` *is* the Ed25519 public key
/// (`fauna_core::identity::ActorId::from(VerifyingKey)`), and an honest leaf's
/// MLS signer IS that key (`build_credential_and_signer`), so for every honest
/// leaf `credential == signature_key` — the same 32 bytes. MLS's
/// `process_message` authenticates only the *signature*; RFC 9420 §5.3/§7.3
/// delegate credential-identity validation to the application. Without this
/// check a patched in-group member could present a leaf whose credential names
/// another actor while signing with its own key, so the MLS-1 sender-bind
/// (which trusts the credential as the authenticated identity) attributes the
/// attacker's messages to the victim — re-opening impersonation + the
/// sender-only-delete-floor bypass one layer down
/// (security review tracked internally, § Part 2; floor:
/// `docs/goal/ui/conversations.md` § Reactions & message delete).
fn validate_credential_binding(credential: &[u8], signature_key: &[u8]) -> Result<()> {
    if credential != signature_key {
        return Err(MlsError::CredentialBindingViolation(
            "MLS leaf credential does not match its signature key (forged actor identity)".into(),
        ));
    }
    Ok(())
}

/// Apply [`validate_credential_binding`] to a single MLS leaf node — used at the
/// KeyPackage-admission (`add_member`/`create_group`) and commit-ingest
/// (`process_commit`) choke points.
fn validate_leaf_binding(leaf: &LeafNode) -> Result<()> {
    validate_credential_binding(
        leaf.credential().serialized_content(),
        leaf.signature_key().as_slice(),
    )
}

/// The 32-byte leaf-credential identity of the member whose commit produced
/// this staged Welcome (the inviter), `None` when openMLS cannot resolve the
/// sender leaf or the credential is not identity-shaped. The join's
/// whole-roster MLS-2 sweep ([`validate_all_member_bindings`]) leaf-verifies
/// this credential along with the rest, so a recorded value is an
/// authenticated ActorId, not an assertion.
fn welcome_sender_identity(staged_welcome: &StagedWelcome) -> Option<[u8; 32]> {
    let leaf = staged_welcome.welcome_sender().ok()?;
    <[u8; 32]>::try_from(leaf.credential().serialized_content()).ok()
}

/// The leaf capabilities every Fauna key package and every own leaf
/// advertises: the room-policy extension type
/// ([`ROOM_POLICY_EXTENSION_TYPE`]) plus whatever `extra` the caller needs
/// (the last-resort marker). Every other capability stays at openMLS's
/// global default (`None`).
fn room_leaf_capabilities(extra: &[ExtensionType]) -> Capabilities {
    let mut extensions = vec![ExtensionType::Unknown(ROOM_POLICY_EXTENSION_TYPE)];
    extensions.extend_from_slice(extra);
    Capabilities::new(None, None, Some(&extensions), None, None)
}

/// The group-context extension set of a policy-bearing room: the policy
/// itself under [`ROOM_POLICY_EXTENSION_TYPE`], and a `required_capabilities`
/// naming that type — the MLS-level negotiation that keeps a leaf which does
/// not enforce the policy out of the room (`crate::room_policy` § *Where the
/// policy lives*). A GroupContextExtensions proposal replaces the whole set,
/// so every policy change re-emits both.
fn room_context_extensions<T>(policy: &RoomPolicyExtension) -> Result<Extensions<T>>
where
    T: openmls::extensions::ExtensionValidator,
    InvalidExtensionError: From<T::Error>,
{
    let bytes = policy.to_bytes()?;
    Extensions::from_vec(vec![
        Extension::Unknown(ROOM_POLICY_EXTENSION_TYPE, UnknownExtension(bytes)),
        Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(
            &[ExtensionType::Unknown(ROOM_POLICY_EXTENSION_TYPE)],
            &[],
            &[],
        )),
    ])
    .map_err(|e| MlsError::OpenMls(format!("room context extensions: {e:?}")))
}

/// What one staged commit does, read off its proposals against the
/// **pre-commit** tree of `group` — the inputs of [`judge_commit`].
fn staged_commit_facts(
    channel: &ChannelId,
    group: &MlsGroup,
    staged_commit: &StagedCommit,
    committer: [u8; 32],
) -> StagedCommitFacts {
    let identity_of_leaf = |index: LeafNodeIndex| -> Option<ActorId> {
        group
            .members()
            .find(|m| m.index == index)
            .and_then(|m| <[u8; 32]>::try_from(m.credential.serialized_content()).ok())
            .map(ActorId)
    };
    let mut facts = StagedCommitFacts {
        channel: channel.0,
        committer: ActorId(committer),
        adds: Vec::new(),
        removes: Vec::new(),
        next_extension: None,
        other_proposals: false,
    };
    for queued in staged_commit.queued_proposals() {
        match queued.proposal() {
            Proposal::Add(add) => {
                match <[u8; 32]>::try_from(
                    add.key_package()
                        .leaf_node()
                        .credential()
                        .serialized_content(),
                ) {
                    Ok(id) => facts.adds.push(ActorId(id)),
                    // An Add of a non-fauna credential has no role to judge
                    // by; the MLS-2 leaf check upstream refuses it anyway.
                    Err(_) => facts.other_proposals = true,
                }
            }
            Proposal::Remove(remove) => match identity_of_leaf(remove.removed()) {
                Some(id) => facts.removes.push(id),
                // A Remove of a leaf the pre-commit tree does not hold is
                // openMLS's to refuse; to the policy it is an unjudgeable
                // shape.
                None => facts.other_proposals = true,
            },
            // A member's own leaf rekey — every member's right.
            Proposal::Update(_) => {}
            Proposal::GroupContextExtensions(gce) => {
                facts.next_extension = Some(match room_policy_in(gce.extensions()) {
                    // The `required_capabilities` naming the extension is the
                    // guard behind property 3 of the module doc and a field
                    // of the same proposed context; a context that keeps the
                    // policy but drops the requirement is as invalid as one
                    // that drops the policy.
                    Some(Ok(_)) if !requires_room_policy(gce.extensions()) => Err(
                        "the commit drops the required_capabilities naming the room policy".into(),
                    ),
                    Some(Ok(next)) => Ok(next),
                    Some(Err(e)) => Err(e.to_string()),
                    None => Err("the commit drops the room-policy extension".into()),
                });
            }
            _ => facts.other_proposals = true,
        }
    }
    facts
}

/// The room-policy extension carried by a group context, decoded and
/// validated; `None` when the context carries none (a policy-less room);
/// `Some(Err(_))` when it carries bytes that do not decode or validate —
/// agreed state every member fails identically, which the commit verdict
/// turns into a refusal rather than an open door.
fn room_policy_in<T>(extensions: &Extensions<T>) -> Option<Result<RoomPolicyExtension>> {
    let UnknownExtension(bytes) = extensions.unknown(ROOM_POLICY_EXTENSION_TYPE)?;
    Some(RoomPolicyExtension::from_bytes(bytes.as_slice()))
}

/// The invite-time half of property 3 (`crate::room_policy` § *Where the
/// policy lives*), enforced by the engine itself: on a policy-bearing group a
/// key package that does not advertise the extension type is refused **by
/// name**, before openMLS's `required_capabilities` check — which is agreed
/// state a patched client could try to strip and
/// which, when it holds, would refuse the same leaf with an opaque error.
/// Every current app's pool package advertises the type, so such a package
/// is a non-conforming (patched or foreign) client's, never an older app's.
fn refuse_unadvertised_leaf_in_policy_room(
    group: &MlsGroup,
    key_package: &KeyPackage,
) -> Result<()> {
    if room_policy_in(group.extensions()).is_some()
        && !MlsEngine::key_package_supports_room_policy(key_package)
    {
        return Err(MlsError::PolicyViolation(format!(
            "member {} does not advertise the room-policy extension and cannot be seated \
             in a policy room",
            hex::encode(key_package.leaf_node().credential().serialized_content())
        )));
    }
    Ok(())
}

/// Whether a group context's `required_capabilities` names the room-policy
/// extension type — the guard that keeps a leaf which does not advertise
/// (and so does not enforce) the policy out of the room.
fn requires_room_policy<T>(extensions: &Extensions<T>) -> bool {
    extensions.required_capabilities().is_some_and(|required| {
        required
            .extension_types()
            .contains(&ExtensionType::Unknown(ROOM_POLICY_EXTENSION_TYPE))
    })
}

/// Apply [`validate_credential_binding`] to every current member of a group —
/// the receiver-side roster sweep run after joining a Welcome, where a patched
/// inviter could have seeded the ratchet tree with a forged-credential leaf.
fn validate_all_member_bindings(group: &MlsGroup) -> Result<()> {
    for member in group.members() {
        validate_credential_binding(
            member.credential.serialized_content(),
            &member.signature_key,
        )?;
    }
    Ok(())
}

/// Nest-side defense-in-depth (security review MLS-2): validate a TLS-serialized
/// KeyPackage at the `fauna.conversations.keypackage.upload` choke point before
/// the nest stores it.
///
/// Parses + validates the KeyPackage (openmls self-signature + structure) and
/// enforces the Fauna leaf-credential binding — its inner credential MUST equal
/// both its leaf signature key AND `expected_actor` (the authenticated uploader's
/// 32-byte ActorId). This stops a patched client from publishing a KeyPackage
/// that claims another actor's identity, so the nest never serves a
/// forged-identity KeyPackage to a peer building a group. The client-side
/// admission checks (`add_member`/`create_group`/`process_commit`/join) are the
/// necessary defense (a patched client can skip the nest); this is the belt.
pub fn verify_uploaded_key_package(
    key_package_bytes: &[u8],
    expected_actor: &[u8; 32],
) -> Result<()> {
    let provider = OpenMlsRustCrypto::default();
    let kp_in = KeyPackageIn::tls_deserialize_exact(key_package_bytes)
        .map_err(|e| MlsError::Encoding(format!("TLS deserialize: {e:?}")))?;
    let kp = kp_in
        .validate(provider.crypto(), ProtocolVersion::Mls10)
        .map_err(|e| MlsError::OpenMls(format!("validate key package: {e:?}")))?;
    let leaf = kp.leaf_node();
    // credential == leaf signature key (the MLS-2 binding) ...
    validate_leaf_binding(leaf)?;
    // ... and both == the authenticated uploader.
    if leaf.credential().serialized_content() != expected_actor.as_slice() {
        return Err(MlsError::CredentialBindingViolation(
            "uploaded KeyPackage credential does not match the authenticated uploader".into(),
        ));
    }
    Ok(())
}

/// Classify an openMLS `UnableToDecrypt` failure on the inbound-commit rail to
/// `(error, keep_memo)`. Factored out of [`MlsEngine::process_commit`] so the
/// mapping is unit-testable directly: the runtime path that yields a
/// `SecretReuseError` needs a forged-`sender_data` handshake message, which
/// openMLS does not let a test craft — so the mapping is exercised here by
/// constructing the `MessageDecryptionError` values themselves.
///
/// Decrypt failures split one nesting level down (the review's "the carve-out
/// was made at exactly one nesting depth"): a library / provider / codec
/// failure inside the secret tree is **local** (same class the top-level
/// `LibraryError` carve-out catches); a garbage or out-of-tolerance ciphertext
/// is **deterministic** on the bytes at this epoch's shared sender-ratchet
/// chain (an in-group attacker can craft one; the honest same-bytes-retry shape
/// is intercepted by the memo).
fn classify_decrypt_error(d: MessageDecryptionError) -> (MlsError, bool) {
    match d {
        MessageDecryptionError::LibraryError(e) => (MlsError::OpenMls(format!("{e:?}")), true),
        // Local ⇒ stall, memo kept. A library / provider / codec failure inside
        // the secret tree is local (the top-level `LibraryError` class, one
        // nesting level down). `SecretReuseError` and `RatchetTypeError` are ALSO
        // device-local — and `SecretReuseError` is load-bearing: openMLS consumes
        // the sender-ratchet generation BEFORE the content AEAD, so a foreign
        // message that burns a generation makes THIS device's own later commit at
        // that generation return `SecretReuseError`. Rounding it to Skipped eats a
        // commit the group applied (silent, attacker-triggerable loss — permanent
        // for a single-device victim); rounding it local ⇒ stall keeps the strand
        // loud + resync-healable, matching the staging layer's own thesis. The
        // memo is hash-keyed on the commit's own bytes, so it catches a same-bytes
        // retry but NOT a *different* message that consumed the generation — hence
        // the split. The fix does not *recover* the
        // burned secret (forward secrecy already deleted it) — it stops the cursor
        // from eating the commit.
        MessageDecryptionError::SecretTreeError(
            st @ (SecretTreeError::LibraryError
            | SecretTreeError::CodecError(_)
            | SecretTreeError::CryptoError(_)
            | SecretTreeError::SecretReuseError
            | SecretTreeError::RatchetTypeError),
        ) => (MlsError::OpenMls(format!("{st:?}")), true),
        // Deterministic on the bytes at this epoch's shared sender-ratchet chain,
        // so every honest member reaches the same verdict (an in-group attacker
        // can still craft one; the honest same-bytes-retry shape is intercepted by
        // the memo). The `SecretTreeError` variants here are wild-generation /
        // bounds errors that return BEFORE any consumption — stalling them would
        // hand an in-group member a cheap wild-generation DoS. `SecretTreeError`
        // is now enumerated exhaustively (no `_`): a future openMLS variant must
        // be classified consciously, not rounded to the destructive answer.
        d @ (MessageDecryptionError::GenerationOutOfBound
        | MessageDecryptionError::AeadError
        | MessageDecryptionError::WrongWireFormat
        | MessageDecryptionError::MalformedContent
        | MessageDecryptionError::SecretTreeError(
            SecretTreeError::TooDistantInThePast
            | SecretTreeError::TooDistantInTheFuture
            | SecretTreeError::IndexOutOfBounds
            | SecretTreeError::RatchetTooLong,
        )) => (MlsError::InvalidCommit(format!("{d:?}")), false),
    }
}

impl MlsEngine {
    /// Create a new MLS engine for the given actor identity.
    ///
    /// This generates the MLS signing keypair, stores it in the provider,
    /// and opens (or creates) the SQLite database at `db_path`.
    #[cfg(feature = "native")]
    pub fn new(identity: ActorKeypair, db_path: &std::path::Path) -> Result<Self> {
        let provider = OpenMlsRustCrypto::default();

        let (credential_with_key, signer) =
            Self::build_credential_and_signer(&identity, &provider)?;

        // Preserve the typed at-rest verdicts across this boundary: an
        // `mls.db` this build is too old to read is intact, not broken, and one
        // another instance is serving is intact *and served* — the caller must
        // be able to tell all three apart (see `MlsError::SchemaIncompatible`
        // and `MlsError::ServedElsewhere`).
        let storage = SqliteStorage::open(db_path).map_err(|e| {
            if e.downcast_ref::<crate::storage::StateServedElsewhere>()
                .is_some()
            {
                return MlsError::ServedElsewhere;
            }
            match e.downcast_ref::<crate::version::SchemaIncompatible>() {
                Some(&crate::version::SchemaIncompatible {
                    db_v,
                    db_min,
                    bin_v,
                }) => MlsError::SchemaIncompatible {
                    db_v,
                    db_min,
                    bin_v,
                },
                None => MlsError::Storage(format!("{e}")),
            }
        })?;

        // Restore provider state from snapshot if available.
        if let Ok(Some(snapshot_bytes)) = storage.load_provider_snapshot()
            && let Ok(values) = decode_provider_snapshot(&snapshot_bytes)
        {
            {
                let mut store = provider.storage().values.write().unwrap();
                *store = values;
            }
            // Re-store the signer so OpenMLS can find it after the storage swap.
            let _ = signer.store(provider.storage());
        }

        // Restore active groups from the database.
        let mut groups = HashMap::new();
        if let Ok(active) = storage.list_active_groups() {
            for (channel_id_bytes, raw_group_id) in &active {
                let group_id = GroupId::from_slice(raw_group_id);
                match MlsGroup::load(provider.storage(), &group_id) {
                    Ok(Some(group)) => {
                        let channel_id = ChannelId::from_group_id(raw_group_id);
                        groups.insert(channel_id, group);
                    }
                    Ok(None) => {
                        // A row naming a group the snapshot holds no byte of.
                        // Swept, not carried: left in place it warned on every
                        // later open over a group that could never load (reachable
                        // by a crash between the separate row and snapshot writes — a
                        // listed group whose provider bytes never landed).
                        tracing::warn!(
                            channel = %ChannelId(*channel_id_bytes),
                            "active group not found in provider storage — sweeping its row"
                        );
                        if let Err(e) = storage.remove_active_group(channel_id_bytes) {
                            tracing::warn!("failed to sweep the stale active-group row: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to load active group: {e:?}");
                    }
                }
            }
        }

        // The last-seen replica listing: the swap's ancestor, which the launch
        // door needs before this process has seen any replica. An unreadable
        // record reads as empty. The swap then carries as it did before the
        // record existed, which keeps a join rather than lose one.
        let replica_listed: HashSet<ChannelId> = storage
            .list_replica_listed()
            .map(|rows| rows.into_iter().map(ChannelId).collect())
            .unwrap_or_else(|e| {
                tracing::warn!("failed to read the last-seen replica listing: {e}");
                HashSet::new()
            });

        // Any pending commit arriving with the restored groups was staged by a
        // previous process whose send died with it — mark it *resumed* so the
        // gate-less crash reconcile can tell it apart from a live stage.
        let resumed: HashSet<ChannelId> = groups
            .iter()
            .filter(|(_, g)| g.pending_commit().is_some())
            .map(|(cid, _)| *cid)
            .collect();

        Ok(Self {
            provider,
            identity,
            credential: credential_with_key,
            signer,
            storage,
            groups: Mutex::new(groups),

            retired: AtomicBool::new(false),
            resumed_pending_channels: Mutex::new(resumed),
            replica_listed: Mutex::new(replica_listed),
        })
    }

    /// Create a new MLS engine without persistent storage.
    ///
    /// Suitable for WASM and other environments where SQLite is unavailable.
    /// Group state is held only in memory and lost when the engine is dropped.
    pub fn new_in_memory(identity: ActorKeypair) -> Result<Self> {
        let provider = OpenMlsRustCrypto::default();

        let (credential_with_key, signer) =
            Self::build_credential_and_signer(&identity, &provider)?;

        #[cfg(feature = "native")]
        let storage =
            SqliteStorage::open_in_memory().map_err(|e| MlsError::Storage(format!("{e}")))?;

        Ok(Self {
            provider,
            identity,
            credential: credential_with_key,
            signer,
            #[cfg(feature = "native")]
            storage,
            groups: Mutex::new(HashMap::new()),

            retired: AtomicBool::new(false),
            resumed_pending_channels: Mutex::new(HashSet::new()),
            replica_listed: Mutex::new(HashSet::new()),
        })
    }

    /// Returns the actor ID for this engine's identity.
    pub fn identity_actor_id(&self) -> ActorId {
        self.identity.actor_id()
    }

    /// Test-only (security review MLS-2): build an in-memory engine whose MLS
    /// leaf `BasicCredential` names `forged_credential` while its leaf signature
    /// key remains this `identity`'s real Ed25519 key — i.e. a leaf that *claims*
    /// another actor but is signed by the attacker. This is the patched-client
    /// leaf the binding check must reject; no honest path produces it
    /// (`build_credential_and_signer` ties credential to signer). Used to drive
    /// the malicious-inviter (join) and self-leaf (commit update-path) vectors.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn new_in_memory_forged_for_test(
        identity: ActorKeypair,
        forged_credential: ActorId,
    ) -> Result<Self> {
        let provider = OpenMlsRustCrypto::default();
        let (mut credential_with_key, signer) =
            Self::build_credential_and_signer(&identity, &provider)?;
        // Override ONLY the credential content; leave `signature_key` = the real
        // signer's key, producing the credential≠signature-key mismatch.
        credential_with_key.credential = BasicCredential::new(forged_credential.0.to_vec()).into();

        #[cfg(feature = "native")]
        let storage =
            SqliteStorage::open_in_memory().map_err(|e| MlsError::Storage(format!("{e}")))?;

        Ok(Self {
            provider,
            identity,
            credential: credential_with_key,
            signer,
            #[cfg(feature = "native")]
            storage,
            groups: Mutex::new(HashMap::new()),

            retired: AtomicBool::new(false),
            resumed_pending_channels: Mutex::new(HashSet::new()),
            replica_listed: Mutex::new(HashSet::new()),
        })
    }

    /// Test-only (security review MLS-2): mint a KeyPackage signed by THIS
    /// engine's key but whose `BasicCredential` names `forged_credential`
    /// instead of this engine's own ActorId — the KeyPackage an attacker's
    /// patched client would publish to be added under a victim's identity.
    /// Returns the TLS-serialized wire bytes (the `keypackage.upload` shape).
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn forge_key_package_bytes_for_test(&self, forged_credential: ActorId) -> Result<Vec<u8>> {
        use openmls::prelude::tls_codec::Serialize as TlsSerialize;
        let forged = CredentialWithKey {
            credential: BasicCredential::new(forged_credential.0.to_vec()).into(),
            signature_key: self.signer.public().into(),
        };
        let kp = KeyPackage::builder()
            .build(CIPHERSUITE, &self.provider, &self.signer, forged)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        kp.key_package()
            .tls_serialize_detached()
            .map_err(|e| MlsError::Encoding(format!("TLS serialize: {e}")))
    }

    /// Test-only (security review MLS-2): the pre-MLS-2 `add_member_from_bytes`
    /// with NO leaf-credential-binding validation — the commit a *patched*
    /// attacker client (which skips the client-side check) would broadcast.
    /// Used to prove an HONEST receiver's `process_commit` rejects a forged leaf
    /// introduced by such a commit. Returns `(commit_bytes, welcome_bytes)`.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn add_member_from_bytes_unchecked_for_test(
        &self,
        channel_id: &ChannelId,
        key_package_bytes: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let kp_in = KeyPackageIn::tls_deserialize_exact(key_package_bytes)
            .map_err(|e| MlsError::Encoding(format!("TLS deserialize: {e:?}")))?;
        let kp = kp_in
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| MlsError::OpenMls(format!("validate key package: {e:?}")))?;
        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        let (commit, welcome, _group_info) = group
            .add_members(&self.provider, &self.signer, std::slice::from_ref(&kp))
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        group
            .merge_pending_commit(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        let commit_bytes = commit
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;
        let welcome_bytes = welcome
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;
        Ok((commit_bytes, welcome_bytes))
    }

    /// Test-only: a key package advertising **no** room-policy capability —
    /// what a non-conforming (patched or foreign) client's pool package looks
    /// like to a policy-bearing room's creator or inviter, so the tests can
    /// pin that such a package is refused by name at birth and at every
    /// later add (no policy-less fallback exists for it).
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn generate_policy_blind_key_package_for_test(&self) -> Result<KeyPackage> {
        self.ensure_live()?;
        let kp = KeyPackage::builder()
            .build(
                CIPHERSUITE,
                &self.provider,
                &self.signer,
                self.credential.clone(),
            )
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        Ok(kp.key_package().clone())
    }

    /// [`Self::generate_policy_blind_key_package_for_test`] as TLS wire bytes
    /// — what such a client's `keypackage.upload` would carry.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn generate_policy_blind_key_package_bytes_for_test(&self) -> Result<Vec<u8>> {
        use openmls::prelude::tls_codec::Serialize as TlsSerialize;
        self.generate_policy_blind_key_package_for_test()?
            .tls_serialize_detached()
            .map_err(|e| MlsError::Encoding(format!("TLS serialize: {e}")))
    }

    /// Test-only: stage a GroupContextExtensions
    /// commit that re-installs `policy` **without** the `required_capabilities`
    /// naming its extension type — the commit a *patched* client would
    /// broadcast to strip the guard that keeps an unadvertised leaf unseatable
    /// (`crate::room_policy` § *Where the policy lives*, property 3). The
    /// honest [`Self::set_room_policy_staged`] always re-emits both
    /// extensions; this door exists so the tests can pin that every honest
    /// member refuses the stripped context.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn set_room_policy_staged_without_requirement_for_test(
        &self,
        channel_id: &ChannelId,
        policy: &RoomPolicyExtension,
    ) -> Result<Vec<u8>> {
        self.ensure_live()?;
        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        let stripped = Extensions::from_vec(vec![Extension::Unknown(
            ROOM_POLICY_EXTENSION_TYPE,
            UnknownExtension(policy.to_bytes()?),
        )])
        .map_err(|e| MlsError::OpenMls(format!("stripped room context extensions: {e:?}")))?;
        let (commit, _welcome, _group_info) = group
            .update_group_context_extensions(&self.provider, stripped, &self.signer)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        let commit_bytes = commit
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;
        self.record_pending_commit_hash(channel_id, &commit_bytes);
        Ok(commit_bytes)
    }

    /// Test-only: delete this
    /// engine's **own private epoch encryption keypairs** for `channel_id`'s
    /// current epoch — the device-local state a torn/stale provider snapshot or
    /// a replica restore can lose while the commit bytes and the group's public
    /// tree stay intact. Staging any update-path commit then fails on *this*
    /// device only; every other member applies the identical bytes cleanly.
    /// Drives the pins proving such a failure classifies *local* (stall), never
    /// intrinsically invalid (skip → the cursor eats a commit the group applied).
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn delete_own_epoch_keypairs_for_test(&self, channel_id: &ChannelId) -> Result<()> {
        use openmls_traits::storage::StorageProvider as _;
        let groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        self.provider
            .storage()
            .delete_encryption_epoch_key_pairs(
                group.group_id(),
                &group.epoch(),
                group.own_leaf_index().u32(),
            )
            .map_err(|e| MlsError::Storage(format!("{e:?}")))
    }

    /// Generate `count` fresh key packages for use in group invitations.
    ///
    /// Every package advertises the room-policy extension type in its leaf
    /// capabilities ([`room_leaf_capabilities`]) so the actor can be seated in
    /// a policy-bearing room; a package without the advertisement (a
    /// non-conforming client's) is refused by name at such a room's birth and
    /// at every add into it — see [`crate::room_policy`] § *Where the policy
    /// lives*.
    pub fn generate_key_packages(&self, count: usize) -> Result<Vec<KeyPackage>> {
        self.ensure_live()?;
        let mut packages = Vec::with_capacity(count);
        for _ in 0..count {
            let kp = KeyPackage::builder()
                .leaf_node_capabilities(room_leaf_capabilities(&[]))
                .build(
                    CIPHERSUITE,
                    &self.provider,
                    &self.signer,
                    self.credential.clone(),
                )
                .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
            packages.push(kp.key_package().clone());
        }
        Ok(packages)
    }

    /// Whether `key_package`'s leaf advertises the room-policy extension type
    /// — i.e. whether the actor it names can be seated in a policy-bearing
    /// room. Every current app's package does; the engine asks it of each
    /// package [`Self::create_group_with_policy`] seats and each one an add
    /// into a policy-bearing room seats, refusing by name a package that does
    /// not. (There is no policy-less fallback for such a package: the
    /// policy-less [`Self::create_group`] serves 1:1 conversations and
    /// folder-share groups, never a group some member could not carry.)
    pub fn key_package_supports_room_policy(key_package: &KeyPackage) -> bool {
        key_package
            .leaf_node()
            .capabilities()
            .extensions()
            .contains(&ExtensionType::Unknown(ROOM_POLICY_EXTENSION_TYPE))
    }

    /// Generate `count` fresh key packages and return them as TLS-serialized byte vectors.
    ///
    /// This is the preferred interface for WASM and FFI callers that cannot
    /// use the `KeyPackage` type directly.
    pub fn generate_key_packages_bytes(&self, count: usize) -> Result<Vec<Vec<u8>>> {
        use openmls::prelude::tls_codec::Serialize as TlsSerialize;
        let packages = self.generate_key_packages(count)?;
        packages
            .iter()
            .map(|kp| {
                kp.tls_serialize_detached()
                    .map_err(|e| MlsError::Encoding(format!("TLS serialize: {e}")))
            })
            .collect()
    }

    /// Mint ONE reusable **last-resort** key package and return it as
    /// TLS-serialized wire bytes.
    ///
    /// Unlike [`Self::generate_key_packages_bytes`] (the consumable one-time
    /// pool), a last-resort key package carries the MLS `last_resort` extension
    /// (`KeyPackageBuilder::mark_as_last_resort`); the nest never deletes it on
    /// `take_key_package`, so a target stays reachable after its one-time pool
    /// drains. Every actor publishes exactly one of these at onboarding (Spec Y2
    /// — `docs/goal/architecture/federation.md` § Key packages — privacy &
    /// exhaustion). Uploaded via `keypackage.upload { last_resort: true }`.
    pub fn generate_last_resort_key_package_bytes(&self) -> Result<Vec<u8>> {
        self.ensure_live()?;
        use openmls::prelude::tls_codec::Serialize as TlsSerialize;
        // The leaf node must *advertise* support for the `last_resort` extension,
        // or `KeyPackageIn::validate` rejects it with `UnsupportedExtension` —
        // plus the room-policy extension every package advertises. Every other
        // capability stays at the global default (`None`).
        let capabilities = room_leaf_capabilities(&[ExtensionType::LastResort]);
        let kp = KeyPackage::builder()
            .leaf_node_capabilities(capabilities)
            .mark_as_last_resort()
            .build(
                CIPHERSUITE,
                &self.provider,
                &self.signer,
                self.credential.clone(),
            )
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        kp.key_package()
            .tls_serialize_detached()
            .map_err(|e| MlsError::Encoding(format!("TLS serialize: {e}")))
    }

    /// Create a new MLS group, adding the given member key packages.
    ///
    /// Returns the derived `ChannelId` and the `MlsMessageOut` containing the
    /// Welcome message that members must process to join.
    pub fn create_group(
        &self,
        member_key_packages: &[KeyPackage],
    ) -> Result<(ChannelId, MlsMessageOut)> {
        self.ensure_live()?;
        // MLS-2: reject any member KeyPackage whose credential ≠ its leaf
        // signature key before admitting it to the new group.
        for kp in member_key_packages {
            validate_leaf_binding(kp.leaf_node())?;
        }

        let mls_group_create_config = MlsGroupCreateConfig::builder()
            .ciphersuite(CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .build();

        // Create the group with ourselves as the sole initial member.
        let mut group = MlsGroup::new(
            &self.provider,
            &self.signer,
            &mls_group_create_config,
            self.credential.clone(),
        )
        .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        // Add the other members.
        let (_commit, welcome, _group_info) = group
            .add_members(&self.provider, &self.signer, member_key_packages)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        // Merge the pending commit so the group advances to the next epoch.
        group
            .merge_pending_commit(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let channel_id = ChannelId::from_group_id(group.group_id().as_slice());

        #[cfg(feature = "native")]
        self.persist_group(&channel_id, &group);

        self.groups
            .lock()
            .expect("lock poisoned")
            .insert(channel_id, group);

        Ok((channel_id, welcome))
    }

    /// Create a new **policy-bearing** MLS group — an end-to-end room whose
    /// group context carries `policy` under [`ROOM_POLICY_EXTENSION_TYPE`]
    /// and whose `required_capabilities` names that type
    /// (`crate::room_policy` § *Where the policy lives*). The creator's own
    /// leaf advertises the type; every member key package must too
    /// ([`Self::key_package_supports_room_policy`]), and one that does not is
    /// refused **by name here** rather than as openMLS's capabilities error.
    /// The refusal is final: the caller surfaces it, never minting the room
    /// policy-less through [`Self::create_group`] instead (that fallback was
    /// an older-app compat remnant, removed 2026-09-25).
    ///
    /// `policy.signed.policy.owner` must be this engine's identity: a room is
    /// born owned by its creator.
    pub fn create_group_with_policy(
        &self,
        member_key_packages: &[KeyPackage],
        policy: &RoomPolicyExtension,
    ) -> Result<(ChannelId, MlsMessageOut)> {
        self.ensure_live()?;
        policy.validate()?;
        if policy.signed.policy.owner != self.identity_actor_id() {
            return Err(MlsError::PolicyViolation(
                "a room is born owned by its creator — the policy names another owner".into(),
            ));
        }
        for kp in member_key_packages {
            validate_leaf_binding(kp.leaf_node())?;
            if !Self::key_package_supports_room_policy(kp) {
                return Err(MlsError::PolicyViolation(format!(
                    "member {} does not advertise the room-policy extension and cannot be \
                     seated in a policy room",
                    hex::encode(kp.leaf_node().credential().serialized_content())
                )));
            }
        }

        let mls_group_create_config = MlsGroupCreateConfig::builder()
            .ciphersuite(CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .capabilities(room_leaf_capabilities(&[]))
            .with_group_context_extensions(room_context_extensions(policy)?)
            .build();

        let mut group = MlsGroup::new(
            &self.provider,
            &self.signer,
            &mls_group_create_config,
            self.credential.clone(),
        )
        .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let (_commit, welcome, _group_info) = group
            .add_members(&self.provider, &self.signer, member_key_packages)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        group
            .merge_pending_commit(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let channel_id = ChannelId::from_group_id(group.group_id().as_slice());

        #[cfg(feature = "native")]
        self.persist_group(&channel_id, &group);

        self.groups
            .lock()
            .expect("lock poisoned")
            .insert(channel_id, group);

        Ok((channel_id, welcome))
    }

    /// Sign `policy` as this engine's identity — the actor key is the MLS
    /// leaf signature key (`validate_leaf_binding`), so the signer members
    /// verify is exactly the leaf that commits the change.
    pub fn sign_room_policy(
        &self,
        policy: &crate::room_policy::RoomPolicy,
    ) -> Result<crate::room_policy::SignedRoomPolicy> {
        policy.sign(&self.identity)
    }

    /// [`Self::sign_room_policy`] for the **community** room `room`, whose
    /// class allows the `request` join rule the end-to-end validation refuses
    /// — and whose versions are bound to it by a room signature
    /// (`crate::room_policy::RoomPolicy::sign_community`).
    pub fn sign_room_policy_community(
        &self,
        room: &[u8; 32],
        policy: &crate::room_policy::RoomPolicy,
    ) -> Result<crate::room_policy::SignedRoomPolicy> {
        policy.sign_community(room, &self.identity)
    }

    /// Sign a community room's **labeler set** as this engine's identity — the
    /// policy's sibling record (`crate::room_policy::RoomLabelers`), behind the
    /// same purpose-bound door as [`Self::sign_room_policy_community`].
    pub fn sign_room_labelers(
        &self,
        labelers: &crate::room_policy::RoomLabelers,
    ) -> Result<crate::room_policy::SignedRoomLabelers> {
        labelers.sign(&self.identity)
    }

    /// Sign a community room's **floor delete record** as this engine's
    /// identity (`crate::room_policy::RoomFloorDelete`) — an owner's or admin's
    /// delete of another member's message at log position `target_seq`, made
    /// under policy version `policy_version`. Same purpose-bound door as
    /// [`Self::sign_room_labelers`]; the author is always this identity.
    pub fn sign_room_floor_delete(
        &self,
        room: &[u8; 32],
        target_seq: u64,
        policy_version: u64,
    ) -> Result<crate::room_policy::SignedRoomFloorDelete> {
        crate::room_policy::RoomFloorDelete {
            room: room.to_vec(),
            target_seq,
            author: self.identity.actor_id(),
            policy_version,
        }
        .sign(&self.identity)
    }

    /// Author and seal one **community-room** message as this engine's
    /// identity — the send twin of [`crate::room_message::open_room_message`].
    ///
    /// A community room has no MLS group, so this engine holds no ratchet for
    /// it; what it does hold is the actor key the class attributes bubbles
    /// with, and that key stays behind this door for the same reason
    /// [`Self::sign_room_policy`] exists rather than an `identity()` getter:
    /// the engine signs *for a purpose*, it does not hand its identity out.
    ///
    /// `gen_key` is the room's **tip** generation and `generation_id` names it
    /// — the caller resolves both from the room's own wraps, which is a
    /// membership fact this engine has no way to know.
    ///
    /// # Errors
    /// As [`crate::room_message::seal_room_message`].
    pub fn seal_room_message(
        &self,
        room: &[u8; 32],
        generation_id: &[u8; 32],
        gen_key: &fauna_core::crypto::GenerationKey,
        sent_at_ms: i64,
        body: crate::types::ChannelMessageBody,
    ) -> Result<Vec<u8>> {
        crate::room_message::seal_room_message(
            gen_key,
            room,
            generation_id,
            &self.identity,
            sent_at_ms,
            body,
        )
    }

    /// Assemble one **community-room generation mint** as this engine's
    /// identity — a fresh generation key, wrapped to every roster member in
    /// `targets` and signed as the minting principal.
    ///
    /// The third door the actor key stays behind, for
    /// [`Self::sign_room_policy`]'s reason. The nest binds a mint's `minter`
    /// to the authenticated caller, so what signs here must be the *actor*
    /// key, not a device key: on this plane the floor's ranks stand in for
    /// the recipient-set scheme's authority fleet
    /// (`conversation-rooms.md` § The three classes → *Community*), which is
    /// also why `authorization` is empty — there is no device-authorization
    /// chain to carry when the authority is a rank on a roster the nest
    /// already holds.
    ///
    /// `parents` is the room's current tip, or empty for its first mint: the
    /// nest admits a mint only when it names the tip it replaces, this plane
    /// having no arbiter to resolve a fork. `targets` is the room's own
    /// roster read back — a mint the members' floor does not cover is refused
    /// at the nest, so the caller reads before it wraps.
    ///
    /// # Errors
    /// [`MlsError::Encoding`] for everything
    /// [`crate::wrapped_blob::group_generation_wraps::build_group_mint`]
    /// refuses — an empty target set, a malformed reception key, or a seal
    /// failure — the variant [`crate::room_message::seal_room_message`] uses
    /// for the same class of failure on the same plane.
    pub fn build_room_generation_mint(
        &self,
        targets: &[fauna_core::group_scope::RosterMember],
        parents: Vec<[u8; 32]>,
        minted_at_ms: i64,
    ) -> Result<crate::wrapped_blob::group_generation_wraps::BuiltGroupMint> {
        crate::wrapped_blob::group_generation_wraps::build_group_mint(
            targets,
            parents,
            // A room's minter has no authority line to mint past.
            Vec::new(),
            self.identity.signing_key(),
            Vec::new(),
            minted_at_ms,
        )
        .map_err(|e| MlsError::Encoding(format!("build room generation mint: {e}")))
    }

    /// Sign a **room invitation** as this engine's identity — the inviter.
    ///
    /// The fourth door the actor key stays behind, and the invitation is the
    /// clearest case for it: the signature is what survives the nest boundary
    /// (`crate::room_policy::RoomInvite` — a cross-nest invite reaches the
    /// invitee through their own home nest, which cannot take the inviting
    /// nest's word for who invited whom), so the key that signs must be the
    /// actor key rather than anything device-scoped.
    ///
    /// Whether this inviter's *rank* permits the invitation is the room's
    /// decision, not this record's: the floor judges it.
    ///
    /// # Errors
    /// As [`crate::room_policy::RoomInvite::sign`] — an invitation naming a
    /// 32-byte-less room, or the `Owner` role (a room has one owner and it is
    /// transferred, never invited).
    pub fn sign_room_invite(
        &self,
        invite: &crate::room_policy::RoomInvite,
    ) -> Result<crate::room_policy::SignedRoomInvite> {
        invite.sign(&self.identity)
    }

    /// Build one **member top-up wrap** as this engine's identity — the healer
    /// — sealing `gen_key` to a newly seated member's roster entry.
    ///
    /// The add side of the scheme's mint triggers: an add never mints
    /// (`account-data-taxonomy.md` § The recipient-set scheme → *Mint
    /// triggers*), it wraps what already exists to the new entry, so this
    /// cannot move a room's tip. The nest binds the healer to the
    /// authenticated caller exactly as it binds a mint's minter, which is why
    /// this signs with the actor key.
    ///
    /// # Errors
    /// [`MlsError::Encoding`] on a malformed reception key or a seal failure —
    /// the variant [`Self::build_room_generation_mint`] uses for the same
    /// class of failure.
    pub fn build_room_topup_wrap(
        &self,
        gen_key: &fauna_core::crypto::GenerationKey,
        target: &fauna_core::group_scope::RosterMember,
        generation_id: &[u8; 32],
        at_ms: i64,
    ) -> Result<fauna_core::group_generation::GroupTopupRecord> {
        crate::wrapped_blob::group_generation_wraps::build_group_topup_wrap(
            gen_key,
            target,
            generation_id,
            self.identity.signing_key(),
            at_ms,
        )
        .map(|(_cell, record)| record)
        .map_err(|e| MlsError::Encoding(format!("build room top-up wrap: {e}")))
    }

    /// Countersign `policy` — which names another member as the room's
    /// owner — as this engine's identity, the outgoing owner, bound to
    /// `channel_id` (`crate::room_policy` § Ownership transfer). The offer
    /// this goes into is the owner's act; the commit is the incoming owner's.
    pub fn countersign_ownership_transfer(
        &self,
        channel_id: &ChannelId,
        policy: &crate::room_policy::RoomPolicy,
    ) -> Result<crate::room_policy::OwnershipCountersignature> {
        policy.countersign_transfer(&channel_id.0, &self.identity)
    }

    /// The room policy `channel_id`'s group context carries, decoded —
    /// `None` for a policy-less room (a 1:1, a folder-share group, or a group
    /// a peer minted with no policy extension) or an unknown channel.
    /// Undecodable agreed bytes surface as the error every member shares.
    pub fn room_policy(&self, channel_id: &ChannelId) -> Option<Result<RoomPolicyExtension>> {
        let groups = self.groups.lock().expect("lock poisoned");
        let group = groups.get(channel_id)?;
        room_policy_in(group.extensions())
    }

    /// Build and **stage** a commit that installs `policy` as the room's
    /// group-context extension (a GroupContextExtensions proposal) without
    /// merging it — the staged twin the membership commits follow
    /// ([`Self::add_member_staged`]: gate-send, then
    /// [`Self::merge_pending_commit`] on accept or
    /// [`Self::clear_pending_commit`] on a stale-epoch rejection). Every
    /// other member judges the change through [`judge_commit`] when it
    /// processes the commit; this side checks only that the room *has* a
    /// policy to change (a policy-less room — a 1:1, a folder-share group, a
    /// group a peer minted without one — cannot acquire one here; see
    /// `crate::room_policy`).
    pub fn set_room_policy_staged(
        &self,
        channel_id: &ChannelId,
        policy: &RoomPolicyExtension,
    ) -> Result<Vec<u8>> {
        self.ensure_live()?;
        policy.validate()?;
        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        if room_policy_in(group.extensions()).is_none() {
            return Err(MlsError::PolicyViolation(
                "a policy-less room carries no policy and cannot acquire one in place".into(),
            ));
        }
        let (commit, _welcome, _group_info) = group
            .update_group_context_extensions(
                &self.provider,
                room_context_extensions(policy)?,
                &self.signer,
            )
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        let commit_bytes = commit
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;
        self.record_pending_commit_hash(channel_id, &commit_bytes);
        Ok(commit_bytes)
    }

    /// Refuse a Welcome **this device holds no addressed key package for**,
    /// naming the case with [`MlsError::NotAddressedToThisDevice`].
    ///
    /// The multi-device steady state, not a fault: every device of an account
    /// consumes every Welcome push, but a device launched *before* a sibling
    /// minted the pool package a Welcome addresses never held that init key
    /// (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync →
    /// *Who may consume a Welcome — any device that holds its key*). Without
    /// this classification the caller cannot tell that expected outcome from a
    /// genuine ingest fault, and every such push logs at `error` on the
    /// long-open device of any two-device account.
    ///
    /// **Asked through our own door.** The question is put to the provider's
    /// own storage — do I hold a key package under any hash ref this Welcome's
    /// secrets address — not to an openMLS error string: a `StagedWelcome`
    /// failure is a diagnostic whose text is the dependency's to change, and
    /// matching on it would make the classification a silent-drift liability
    /// (the same reason `process_commit` classifies each openMLS variant
    /// explicitly rather than by message).
    ///
    /// Errs on the side of **proceeding**: a storage failure while answering is
    /// not itself a not-addressed verdict, so it surfaces as
    /// [`MlsError::Storage`] rather than misreporting a real fault as the
    /// benign case.
    fn ensure_addressed_to_this_device(&self, welcome: &Welcome) -> Result<()> {
        use openmls_traits::storage::StorageProvider as _;
        for secrets in welcome.secrets() {
            let held = self
                .provider
                .storage()
                .key_package::<_, KeyPackageBundle>(&secrets.new_member())
                .map_err(|e| MlsError::Storage(format!("read key package: {e:?}")))?;
            if held.is_some() {
                return Ok(());
            }
        }
        Err(MlsError::NotAddressedToThisDevice)
    }

    /// Join an existing group from a Welcome message.
    ///
    /// Returns the `ChannelId` of the joined group.
    pub fn join_from_welcome(&self, welcome: MlsMessageOut) -> Result<ChannelId> {
        // A join mutates group state (both `StagedWelcome` calls write the
        // in-memory provider, and the group lands in `self.groups`), so the
        // quiesce contract binds it — `account-data-plane.md`
        // § Multi-instance concurrency: `is_retired` gates every method that
        // mutates group state. The refusal must be an `Err`: the durable
        // inbox acks on `Ok`, so a retired engine that "succeeded" here would
        // consume the Welcome, drop the nest's durable row, and leave the
        // successor a member of a group no engine of theirs can open.
        self.ensure_live()?;
        let mls_group_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();

        // Serialize and re-deserialize through TLS codec to convert
        // MlsMessageOut → MlsMessageIn (the direct conversion is only
        // available behind the test-utils feature).
        let welcome_bytes = welcome
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;
        let welcome_in = MlsMessageIn::tls_deserialize_exact(&welcome_bytes)
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;
        let welcome = match welcome_in.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => return Err(MlsError::OpenMls("expected a Welcome message".into())),
        };

        // Classified before staging, for the same reason the sibling door is:
        // a Welcome no key of ours is addressed by is the multi-device steady
        // state, and it must not read as an ingest fault.
        self.ensure_addressed_to_this_device(&welcome)?;

        let staged_welcome = StagedWelcome::new_from_welcome(
            &self.provider,
            &mls_group_config,
            welcome,
            None, // ratchet tree comes from the extension
        )
        .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        // Read the Welcome's sender before `into_group` consumes the staging
        // (recorded durably below, once the roster validation has run).
        let sender_identity = welcome_sender_identity(&staged_welcome);

        let group = staged_welcome
            .into_group(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        // MLS-2: a patched inviter could seed the ratchet tree with a
        // forged-credential leaf (naming another actor). Validate the whole
        // learned roster before trusting/persisting it; reject the join on a
        // mismatch rather than materialize an impersonated member.
        validate_all_member_bindings(&group)?;

        let channel_id = ChannelId::from_group_id(group.group_id().as_slice());
        self.record_welcome_sender(&channel_id, sender_identity, group.epoch().as_u64());

        #[cfg(feature = "native")]
        self.persist_group(&channel_id, &group);

        self.groups
            .lock()
            .expect("lock poisoned")
            .insert(channel_id, group);

        Ok(channel_id)
    }

    /// Create a new MLS group with just the creator (solo group).
    ///
    /// Members can be added later via `add_member`. Returns the `ChannelId`.
    pub fn create_solo_group(&self) -> Result<ChannelId> {
        self.ensure_live()?;
        let mls_group_create_config = MlsGroupCreateConfig::builder()
            .ciphersuite(CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .build();

        let group = MlsGroup::new(
            &self.provider,
            &self.signer,
            &mls_group_create_config,
            self.credential.clone(),
        )
        .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let channel_id = ChannelId::from_group_id(group.group_id().as_slice());

        #[cfg(feature = "native")]
        self.persist_group(&channel_id, &group);

        self.groups
            .lock()
            .expect("lock poisoned")
            .insert(channel_id, group);

        Ok(channel_id)
    }

    /// The raw MLS group id for a channel, if this engine holds the group.
    ///
    /// `ChannelId` is a one-way `derive_key` of the group id, so the group id
    /// can't be recovered from it; group bootstrap needs the raw id to populate
    /// the `Group { group_id }` welcome kind
    /// (`fauna.conversations.welcome.deliver`).
    pub fn group_id_bytes(&self, channel_id: &ChannelId) -> Option<Vec<u8>> {
        self.groups
            .lock()
            .expect("lock poisoned")
            .get(channel_id)
            .map(|g| g.group_id().as_slice().to_vec())
    }

    /// Add a new member from raw TLS-encoded key package bytes.
    ///
    /// Deserializes and validates the key package internally (where the crypto
    /// provider lives), then delegates to `add_member`. Returns the serialized
    /// commit bytes and welcome bytes.
    pub fn add_member_from_bytes(
        &self,
        channel_id: &ChannelId,
        key_package_bytes: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let kp_in = KeyPackageIn::tls_deserialize_exact(key_package_bytes)
            .map_err(|e| MlsError::Encoding(format!("TLS deserialize: {e:?}")))?;
        let kp = kp_in
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| MlsError::OpenMls(format!("validate key package: {e:?}")))?;
        let (commit_bytes, welcome) = self.add_member(channel_id, &kp)?;
        let welcome_bytes = welcome
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;
        Ok((commit_bytes, welcome_bytes))
    }

    /// Deserialize and validate a TLS-encoded key package into a `KeyPackage`.
    ///
    /// The inbound counterpart to [`Self::generate_key_packages_bytes`]: group
    /// bootstrap (`fauna-conversations::FaunaMlsBackend::send` new-thread path)
    /// fetches each peer's key-package bytes over the wire
    /// (`fauna.conversations.keypackage.fetch`) and needs a validated
    /// `KeyPackage` to hand to [`Self::create_group`]. Validation runs against
    /// the crypto provider (where the provider lives), mirroring the internal
    /// deserialize+validate in [`Self::add_member_from_bytes`].
    pub fn key_package_from_bytes(&self, key_package_bytes: &[u8]) -> Result<KeyPackage> {
        let kp_in = KeyPackageIn::tls_deserialize_exact(key_package_bytes)
            .map_err(|e| MlsError::Encoding(format!("TLS deserialize: {e:?}")))?;
        kp_in
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| MlsError::OpenMls(format!("validate key package: {e:?}")))
    }

    /// Join an existing group from raw Welcome bytes.
    ///
    /// Deserializes the Welcome message from TLS-encoded bytes and joins
    /// the group. Returns the `ChannelId`.
    pub fn join_from_welcome_bytes(&self, welcome_bytes: &[u8]) -> Result<ChannelId> {
        // Guarded in its own right, NOT by the sibling above: this is a
        // duplicate join body, not a delegate to `join_from_welcome`, so a
        // single guard there would leave the door the inbox drain actually
        // takes wide open. Same contract, same reason.
        self.ensure_live()?;
        let mls_group_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();

        let mls_msg_in = MlsMessageIn::tls_deserialize_exact(welcome_bytes)
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;
        let welcome = match mls_msg_in.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => return Err(MlsError::OpenMls("expected a Welcome message".into())),
        };

        // Guarded here as well as in the sibling above, and for the reason the
        // duplicate-body comment gives: this is the door the push arm and the
        // durable drain actually take, so a classification only there would
        // leave the production path unclassified.
        self.ensure_addressed_to_this_device(&welcome)?;

        let staged_welcome =
            StagedWelcome::new_from_welcome(&self.provider, &mls_group_config, welcome, None)
                .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        // Read the Welcome's sender before `into_group` consumes the staging
        // (recorded durably below, once the roster validation has run).
        let sender_identity = welcome_sender_identity(&staged_welcome);

        let group = staged_welcome
            .into_group(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        // MLS-2: a patched inviter could seed the ratchet tree with a
        // forged-credential leaf (naming another actor). Validate the whole
        // learned roster before trusting/persisting it; reject the join on a
        // mismatch rather than materialize an impersonated member.
        validate_all_member_bindings(&group)?;

        let channel_id = ChannelId::from_group_id(group.group_id().as_slice());
        self.record_welcome_sender(&channel_id, sender_identity, group.epoch().as_u64());

        #[cfg(feature = "native")]
        self.persist_group(&channel_id, &group);

        self.groups
            .lock()
            .expect("lock poisoned")
            .insert(channel_id, group);

        Ok(channel_id)
    }

    /// Durably record the Welcome sender's leaf-verified identity for a
    /// just-joined channel (see [`WELCOME_SENDER_PREFIX`]). Runs after the
    /// join's MLS-2 whole-roster validation and before the join's
    /// `persist_group`, so the record rides the same snapshot as the group.
    ///
    /// With it, the **history admission** ([`HISTORY_ADMISSION_PREFIX`]): the
    /// epoch this device joined at. Only beside a resolved sender — an
    /// admission nobody can be matched against is one nobody may spend.
    fn record_welcome_sender(
        &self,
        channel_id: &ChannelId,
        sender_identity: Option<[u8; 32]>,
        joined_epoch: u64,
    ) {
        if let Some(id) = sender_identity {
            let mut values = self.provider.storage().values.write().unwrap();
            values.insert(welcome_sender_key(channel_id), id.to_vec());
            values.insert(
                history_admission_key(channel_id),
                joined_epoch.to_be_bytes().to_vec(),
            );
        }
    }

    /// Encrypt a `ChannelMessage` for the given channel.
    ///
    /// The message is dag-cbor-encoded, then encrypted via the MLS group, and
    /// the resulting ciphertext is TLS-serialized to bytes.
    pub fn encrypt(&self, channel_id: &ChannelId, message: &ChannelMessage) -> Result<Vec<u8>> {
        let message_bytes = fauna_cbor::encode_canonical(message)
            .map_err(|e| MlsError::Encoding(format!("{e}")))?;
        self.encrypt_payload(channel_id, &message_bytes)
    }

    /// Test-only: seal arbitrary application bytes as this member's MLS
    /// application message — the payload a LATER build would post, which
    /// [`Self::encrypt`] cannot produce because this build's
    /// [`ChannelMessage`] has no variant for it. What the record-level skip
    /// pins drive the poll loop with (`ChannelMessageBody` is ruled skip:
    /// an unknown body fails that one record's decode).
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn encrypt_payload_for_test(
        &self,
        channel_id: &ChannelId,
        payload: &[u8],
    ) -> Result<Vec<u8>> {
        self.encrypt_payload(channel_id, payload)
    }

    fn encrypt_payload(&self, channel_id: &ChannelId, message_bytes: &[u8]) -> Result<Vec<u8>> {
        self.ensure_live()?;
        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;

        let mls_out = group
            .create_message(&self.provider, &self.signer, message_bytes)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let ciphertext = mls_out
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;

        Ok(ciphertext)
    }

    /// Decrypt a ciphertext into a `ChannelMessage` for the given channel.
    ///
    /// The ciphertext is TLS-deserialized, processed by the MLS group, then
    /// dag-cbor-decoded back into a `ChannelMessage`.
    pub fn decrypt(&self, channel_id: &ChannelId, ciphertext: &[u8]) -> Result<ChannelMessage> {
        self.decrypt_authenticated(channel_id, ciphertext)
            .map(|decrypted| decrypted.message)
    }

    /// [`Self::decrypt`], plus what only this moment can vouch for
    /// ([`DecryptedMessage`]): the **epoch the message was sealed in** and the
    /// authenticated sender's **role under the policy of that epoch** — what a
    /// governed room's owner/admin delete is judged by (`conversation-rooms.md`
    /// § Roles and authorization → *Delete any message — the mechanism*).
    ///
    /// A `None` role is "no role to vouch for", and a caller must fail closed on it:
    /// a policy-less room (no policy in the group context), an unreadable policy,
    /// or a message from an epoch other than the one this group now holds —
    /// the group context in hand is then not the one the message was made
    /// under, and judging by it would let a seat that folded late disagree
    /// with one that folded on time. The epoch is read off the MLS framing,
    /// which `process_message` authenticates, never off the payload.
    pub fn decrypt_authenticated(
        &self,
        channel_id: &ChannelId,
        ciphertext: &[u8],
    ) -> Result<DecryptedMessage> {
        self.ensure_live()?;
        let mls_msg_in = MlsMessageIn::tls_deserialize_exact(ciphertext)
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;

        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;

        let protocol_msg: ProtocolMessage = mls_msg_in
            .try_into_protocol_message()
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let epoch = protocol_msg.epoch().as_u64();
        let sealed_in_held_epoch = protocol_msg.epoch() == group.epoch();
        let processed = group
            .process_message(&self.provider, protocol_msg)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        // Bind the message sender to the MLS-*authenticated* leaf credential.
        // `process_message` verifies the ciphertext against the sending leaf's
        // credential; the inner `ChannelMessage.sender` is opaque to MLS and
        // fully attacker-controllable (an in-group member runs a patched client
        // and writes any victim id into the payload before sealing). We recover
        // the authenticated identity here — the credential's serialized content
        // is the 32-byte `ActorId`, exactly as `group_members()` reads member
        // credentials — and overwrite the self-asserted field before returning.
        // Without this, an in-group member could impersonate any member on the
        // chat rail and bypass the sender-only-delete floor (`conversations.md`
        // § Reactions & message delete).
        let authenticated_sender =
            <[u8; 32]>::try_from(processed.credential().serialized_content())
                .map(ActorId)
                .map_err(|_| {
                    MlsError::PolicyViolation(
                        "authenticated sender credential is not a 32-byte ActorId".into(),
                    )
                })?;

        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app_msg) => {
                let mut channel_message: ChannelMessage =
                    fauna_cbor::decode_strict(app_msg.into_bytes().as_slice())
                        .map_err(|e| MlsError::Encoding(format!("{e}")))?;
                channel_message.sender = authenticated_sender;
                let sender_role = sealed_in_held_epoch
                    .then(|| room_policy_in(group.extensions()))
                    .flatten()
                    .and_then(|policy| policy.ok())
                    .map(|policy| policy.role_of(&authenticated_sender));
                Ok(DecryptedMessage {
                    message: channel_message,
                    sender_role,
                    epoch,
                })
            }
            other => Err(MlsError::OpenMls(format!(
                "expected ApplicationMessage, got: {other:?}"
            ))),
        }
    }

    /// Add a new member to an existing group.
    ///
    /// Returns the serialized commit bytes and the Welcome message for the new
    /// member.
    pub fn add_member(
        &self,
        channel_id: &ChannelId,
        key_package: &KeyPackage,
    ) -> Result<(Vec<u8>, MlsMessageOut)> {
        self.ensure_live()?;
        // MLS-2: reject a KeyPackage whose credential ≠ its leaf signature key.
        validate_leaf_binding(key_package.leaf_node())?;

        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        refuse_unadvertised_leaf_in_policy_room(group, key_package)?;
        // Keep the epoch this commit leaves, before anything moves it.
        self.remember_room_post_secret(channel_id, group);

        let (commit, welcome, _group_info) = group
            .add_members(
                &self.provider,
                &self.signer,
                std::slice::from_ref(key_package),
            )
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        // Merge the pending commit.
        group
            .merge_pending_commit(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let commit_bytes = commit
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;

        Ok((commit_bytes, welcome))
    }

    /// Remove a member from a group by their leaf index.
    ///
    /// Returns the serialized commit bytes.
    pub fn remove_member(&self, channel_id: &ChannelId, leaf_index: u32) -> Result<Vec<u8>> {
        self.remove_members(channel_id, &[leaf_index])
    }

    /// Remove several leaves in **one** commit.
    ///
    /// One commit rather than a loop of [`Self::remove_member`], because each
    /// commit advances the epoch: removing N leaves one at a time asks every
    /// remaining member to process N epoch changes for a single membership
    /// decision, and leaves the group readable by the not-yet-removed leaves for
    /// the epochs in between. Its caller is
    /// `succession::commit_remove_old`, which must evict every leaf bearing the
    /// succeeded credential at once.
    ///
    /// An empty slice is a caller bug — MLS has no empty-removal commit — and is
    /// refused by name rather than handed to OpenMLS.
    pub fn remove_members(&self, channel_id: &ChannelId, leaf_indices: &[u32]) -> Result<Vec<u8>> {
        self.ensure_live()?;
        if leaf_indices.is_empty() {
            return Err(MlsError::PolicyViolation(
                "remove_members was called with no leaves — there is no empty-removal commit"
                    .into(),
            ));
        }
        let leaves: Vec<LeafNodeIndex> = leaf_indices
            .iter()
            .copied()
            .map(LeafNodeIndex::new)
            .collect();

        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        // Keep the epoch this commit leaves, before anything moves it.
        self.remember_room_post_secret(channel_id, group);

        let (commit, _welcome, _group_info) = group
            .remove_members(&self.provider, &self.signer, &leaves)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        // Merge the pending commit.
        group
            .merge_pending_commit(&self.provider)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let commit_bytes = commit
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;

        Ok(commit_bytes)
    }

    // ── Device-owned-epoch rebase primitives ────────────────────────────────
    //
    // The gated twins of the commit-producing flows above, plus the takeover
    // `self_update`, split the "build a commit" step from the "merge it" step so
    // the caller can gate-send the commit and only merge it once the nest accepts
    // (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync;
    // design tracked internally, §3). The conversation membership path drives
    // these staged variants on BOTH
    // its branches — through the `fauna.conversations.channel.stale` gate when a
    // `CommitGate` is injected, bare stage→send→merge-on-accept when not
    // (devices.md Rule 1: merge only after the commit bytes are durable on the
    // log). The optimistic merging `add_member`/`remove_member` above must not
    // gain new production callers on an epoch-advancing path; they remain for
    // the dormant `DeviceSyncChannel` primitive and recovery/test flows.

    /// Build and **stage** an `add_member` commit *without merging it* — the
    /// gated twin of [`Self::add_member`]. The caller gate-sends the returned
    /// commit, then [`Self::merge_pending_commit`] on accept or
    /// [`Self::clear_pending_commit`] on a `fauna.conversations.channel.stale`
    /// rejection. Returns `(commit_bytes, welcome)`; the group carries a pending
    /// commit until merged or cleared — the step-1 state the takeover sequence
    /// relies on.
    ///
    /// **Durability.** The pending lives in the provider KV, and that KV is
    /// *in-memory* (`OpenMlsRustCrypto`'s `MemoryStorage`). It survives a crash
    /// only once the KV has been snapshotted somewhere durable — in practice the
    /// gate's step-2 `save_provider_snapshot` CAS-put of the `provider` replica
    /// (`fauna_client_mls_sync::MlsStateSync::send_commit_gated`). The KV is not
    /// itself a durable store: [`Self::save_state`] is the only local snapshot
    /// primitive, and it is reached solely from [`Self::persist_group`] — i.e. on
    /// group create/join, never on an epoch advance. A caller that stages without
    /// taking one of those snapshots therefore has **no** crash-durable pending.
    pub fn add_member_staged(
        &self,
        channel_id: &ChannelId,
        key_package: &KeyPackage,
    ) -> Result<(Vec<u8>, MlsMessageOut)> {
        self.ensure_live()?;
        // MLS-2: reject a KeyPackage whose credential ≠ its leaf signature key.
        validate_leaf_binding(key_package.leaf_node())?;

        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        refuse_unadvertised_leaf_in_policy_room(group, key_package)?;

        let (commit, welcome, _group_info) = group
            .add_members(
                &self.provider,
                &self.signer,
                std::slice::from_ref(key_package),
            )
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let commit_bytes = commit
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;

        self.record_pending_commit_hash(channel_id, &commit_bytes);
        Ok((commit_bytes, welcome))
    }

    /// Deserialize + validate a TLS-encoded key package, then **stage** an
    /// `add_member` commit without merging it — the from-bytes twin of
    /// [`Self::add_member_staged`], returning the Welcome as *bytes* (like
    /// [`Self::add_member_from_bytes`], the optimistic twin). The gated
    /// conversation add path (`fauna_client_mls_sync::FaunaCommitGate`) fetches
    /// each new member's key-package bytes over the wire and needs the Welcome as
    /// bytes to `welcome_deliver`; serializing the `MlsMessageOut` here keeps all
    /// openmls codec inside `fauna-mls`. Merge/clear discipline is
    /// [`Self::add_member_staged`]'s: gate-send, then [`Self::merge_pending_commit`]
    /// on accept or [`Self::clear_pending_commit`] on a stale-epoch rejection.
    pub fn add_member_staged_from_bytes(
        &self,
        channel_id: &ChannelId,
        key_package_bytes: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let kp = self.key_package_from_bytes(key_package_bytes)?;
        let (commit_bytes, welcome) = self.add_member_staged(channel_id, &kp)?;
        let welcome_bytes = welcome
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;
        Ok((commit_bytes, welcome_bytes))
    }

    /// Build and **stage** a `remove_member` commit *without merging it* — the
    /// gated twin of [`Self::remove_member`]. See [`Self::add_member_staged`] for
    /// the merge/clear discipline.
    pub fn remove_member_staged(&self, channel_id: &ChannelId, leaf_index: u32) -> Result<Vec<u8>> {
        self.ensure_live()?;
        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;

        let (commit, _welcome, _group_info) = group
            .remove_members(
                &self.provider,
                &self.signer,
                &[LeafNodeIndex::new(leaf_index)],
            )
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let commit_bytes = commit
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;

        self.record_pending_commit_hash(channel_id, &commit_bytes);
        Ok(commit_bytes)
    }

    /// Build and **stage** a self-`Update` commit (own-leaf rekey) without
    /// merging it — the device **takeover** primitive of the device-owned-epoch
    /// invariant. A device that did not author the channel's current epoch posts
    /// this before its first application send, bumping the epoch to a fresh secret
    /// tree so a shared single leaf never forks a ratchet generation (design
    /// constraint 2 / §3c). Gated like the staged membership commits: gate-send,
    /// then [`Self::merge_pending_commit`] on accept or
    /// [`Self::clear_pending_commit`] on a stale-epoch rejection. Returns the
    /// serialized commit bytes; the group carries a pending commit until merged
    /// or cleared (openmls errors if a second commit is built over an unmerged
    /// one — the rebase always merges or clears first).
    pub fn self_update(&self, channel_id: &ChannelId) -> Result<Vec<u8>> {
        self.ensure_live()?;
        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;

        // The takeover re-advertises this leaf's current capabilities
        // (`room_leaf_capabilities`), so what a leaf advertises never falls
        // behind what this build supports (`crate::room_policy`).
        let leaf_parameters = LeafNodeParameters::builder()
            .with_capabilities(room_leaf_capabilities(&[]))
            .build();
        let bundle = group
            .self_update(&self.provider, &self.signer, leaf_parameters)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;

        let commit_bytes = bundle
            .commit()
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("{e:?}")))?;

        self.record_pending_commit_hash(channel_id, &commit_bytes);
        Ok(commit_bytes)
    }

    /// Merge the group's own pending commit — the **accept** leg of the rebase,
    /// run once the nest accepts the gate-send. Advances the group to the
    /// committed epoch. openmls errors if there is no pending commit, so call it
    /// only after a staged commit was accepted.
    pub fn merge_pending_commit(&self, channel_id: &ChannelId) -> Result<()> {
        self.ensure_live()?;
        {
            let mut groups = self.groups.lock().expect("lock poisoned");
            let group = groups
                .get_mut(channel_id)
                .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
            // Keep the epoch this merge leaves.
            self.remember_room_post_secret(channel_id, group);
            group
                .merge_pending_commit(&self.provider)
                .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        }
        // The pending is gone — its identity must go with it, in the same
        // provider KV the merge just rewrote, so a snapshot taken after this
        // point never claims a pending the group no longer carries.
        self.forget_pending_commit_hash(channel_id);
        Ok(())
    }

    /// Whether the group currently carries a **staged, unmerged** pending commit
    /// (its own — built by one of the `*_staged` primitives or [`Self::self_update`]
    /// and neither merged nor cleared yet). `false` for an unknown channel — a
    /// group this engine doesn't hold has nothing pending, mirroring the
    /// forgotten-group tolerance of the folder resume path. Lets a resume /
    /// recovery path decide between "merge the restored pending" and "nothing to
    /// merge" without triggering openmls' no-pending error
    /// (`devices.md` § Cross-device MLS group-state sync, Rule 1).
    pub fn has_pending_commit(&self, channel_id: &ChannelId) -> bool {
        let groups = self.groups.lock().expect("lock poisoned");
        groups
            .get(channel_id)
            .is_some_and(|group| group.pending_commit().is_some())
    }

    /// The **commit identity** — `blake3` of the wire commit bytes — of the
    /// channel's staged, unmerged pending commit, or `None` when nothing is
    /// pending (or the pending carries no stamp — see
    /// [`PENDING_COMMIT_HASH_PREFIX`]).
    ///
    /// Stamped by every staging primitive ([`Self::add_member_staged`],
    /// [`Self::remove_member_staged`], [`Self::self_update`]) and cleared by
    /// [`Self::merge_pending_commit`] / [`Self::clear_pending_commit`], so it
    /// tracks `has_pending_commit` exactly and survives a restart with it.
    ///
    /// A resume path that holds durable commit *bytes* uses this to prove the
    /// pending it is about to merge is **that** commit rather than a sibling's:
    /// openMLS cannot re-derive wire bytes from a `StagedCommit`, so without
    /// this stamp `has_pending_commit` alone cannot distinguish "my commit" from
    /// "some other commit staged on this channel". Merging the latter while
    /// broadcasting the former forks the group.
    ///
    /// `Some(h)` with `h != blake3(my_bytes)` means a **foreign** pending: the
    /// caller must refuse to merge, never clear it (it may be the sibling's only
    /// merge source), and retry once the sibling completes.
    pub fn pending_commit_hash(&self, channel_id: &ChannelId) -> Option<[u8; 32]> {
        // Only report an identity while a pending actually exists: a stale stamp
        // (one left over after its pending was merged or cleared without
        // clearing the stamp) must never be read as a live pending.
        if !self.has_pending_commit(channel_id) {
            return None;
        }
        let values = self.provider.storage().values.read().unwrap();
        values
            .get(&pending_commit_hash_key(channel_id))
            .and_then(|v| <[u8; 32]>::try_from(v.as_slice()).ok())
    }

    /// Whether `channel_id`'s staged pending commit was **reloaded from the
    /// provider snapshot** (staged by a previous process) rather than staged by
    /// this one — see the field doc on `resumed_pending_channels`. Only a
    /// resumed pending may be cleared by the poll-path crash reconcile after a
    /// full log walk finds no matching own commit; `false` the moment any
    /// in-process stage / merge / clear touches the channel.
    pub fn has_resumed_pending(&self, channel_id: &ChannelId) -> bool {
        self.resumed_pending_channels
            .lock()
            .expect("lock poisoned")
            .contains(channel_id)
            && self.has_pending_commit(channel_id)
    }

    /// Stamp `commit_bytes`' identity as `channel_id`'s pending-commit identity.
    /// Called by each staging primitive with the exact bytes it returns.
    fn record_pending_commit_hash(&self, channel_id: &ChannelId, commit_bytes: &[u8]) {
        let hash = *blake3::hash(commit_bytes).as_bytes();
        let mut values = self.provider.storage().values.write().unwrap();
        values.insert(pending_commit_hash_key(channel_id), hash.to_vec());
        // Staged by THIS process — whatever "resumed" state the channel had is
        // superseded (openmls forbids staging over an unmerged pending, so a
        // resumed pending was merged or cleared before reaching here).
        self.resumed_pending_channels
            .lock()
            .expect("lock poisoned")
            .remove(channel_id);
    }

    /// Drop `channel_id`'s pending-commit identity — the pending is gone (merged,
    /// cleared, or the group forgotten). Idempotent.
    fn forget_pending_commit_hash(&self, channel_id: &ChannelId) {
        let mut values = self.provider.storage().values.write().unwrap();
        values.remove(&pending_commit_hash_key(channel_id));
        self.resumed_pending_channels
            .lock()
            .expect("lock poisoned")
            .remove(channel_id);
    }

    /// `channel_id`'s merge-failed-commit memo — `(blake3 of the wire commit
    /// bytes, group epoch it failed at)` — or `None` when the channel has no
    /// unresolved post-consumption failure (see [`MERGE_FAILED_COMMIT_PREFIX`]).
    fn merge_failed_commit_memo(&self, channel_id: &ChannelId) -> Option<([u8; 32], u64)> {
        let values = self.provider.storage().values.read().unwrap();
        let v = values.get(&merge_failed_commit_key(channel_id))?;
        if v.len() != 40 {
            return None;
        }
        let hash: [u8; 32] = v[..32].try_into().ok()?;
        let epoch = u64::from_be_bytes(v[32..40].try_into().ok()?);
        Some((hash, epoch))
    }

    /// Arm `channel_id`'s merge-failed-commit memo for `commit_hash` at
    /// `group_epoch` (see [`MERGE_FAILED_COMMIT_PREFIX`] for when).
    fn record_merge_failed_commit(
        &self,
        channel_id: &ChannelId,
        commit_hash: [u8; 32],
        group_epoch: u64,
    ) {
        let mut v = Vec::with_capacity(40);
        v.extend_from_slice(&commit_hash);
        v.extend_from_slice(&group_epoch.to_be_bytes());
        let mut values = self.provider.storage().values.write().unwrap();
        values.insert(merge_failed_commit_key(channel_id), v);
    }

    /// Drop `channel_id`'s merge-failed-commit memo (commit merged, verdict was
    /// intrinsically-invalid / pre-consumption, or the entry went stale).
    /// Idempotent.
    fn forget_merge_failed_commit(&self, channel_id: &ChannelId) {
        let mut values = self.provider.storage().values.write().unwrap();
        values.remove(&merge_failed_commit_key(channel_id));
    }

    /// Durably mark `channel_id` as a **chat** channel (see
    /// [`CHANNEL_KIND_CHAT_PREFIX`] — the folder sweep must never treat an
    /// unbound chat group as a folder channel). Idempotent; never cleared.
    pub fn mark_channel_chat(&self, channel_id: &ChannelId) {
        let mut values = self.provider.storage().values.write().unwrap();
        values.insert(channel_kind_chat_key(channel_id), b"1".to_vec());
    }

    /// Whether `channel_id` carries the durable chat marker
    /// ([`Self::mark_channel_chat`]). `false` for a channel with no marker (a folder channel, or a chat group
    /// before its bind stamps it) — callers must treat `false` as
    /// "unknown", never as "provably not chat".
    pub fn is_channel_chat(&self, channel_id: &ChannelId) -> bool {
        let values = self.provider.storage().values.read().unwrap();
        values.contains_key(&channel_kind_chat_key(channel_id))
    }

    /// Durably mark `channel_id` as a one-off **scheduling** delivery (see
    /// [`CHANNEL_KIND_SCHEDULING_PREFIX`]). Idempotent; never cleared.
    /// [`Self::build_scheduling_delivery`] stamps it for every organizer; the
    /// conversations backend's welcome-ingest stamps it for the recipient.
    pub fn mark_channel_scheduling(&self, channel_id: &ChannelId) {
        let mut values = self.provider.storage().values.write().unwrap();
        values.insert(channel_kind_scheduling_key(channel_id), b"1".to_vec());
    }

    /// Whether `channel_id` carries the durable scheduling marker
    /// ([`Self::mark_channel_scheduling`]). Like [`Self::is_channel_chat`],
    /// `false` on an unmarked channel means "unknown", never "provably not
    /// scheduling".
    pub fn is_channel_scheduling(&self, channel_id: &ChannelId) -> bool {
        let values = self.provider.storage().values.read().unwrap();
        values.contains_key(&channel_kind_scheduling_key(channel_id))
    }

    /// Durably record `channel_id`'s **folder owner** (see
    /// [`FOLDER_CHANNEL_OWNER_PREFIX`]) — arming the owner-managed-roster
    /// commit policy in [`Self::process_commit`]. Stamped owner-side at
    /// folder-group mint and member-side at folder-Welcome join (from the
    /// MLS-authenticated Welcome sender). Idempotent; never cleared. Persists
    /// the provider snapshot on native so the marker is durable from the stamp,
    /// not from the next incidental save.
    pub fn mark_folder_channel_owner(&self, channel_id: &ChannelId, owner: &ActorId) {
        {
            let mut values = self.provider.storage().values.write().unwrap();
            values.insert(folder_channel_owner_key(channel_id), owner.0.to_vec());
        }
        #[cfg(feature = "native")]
        if let Err(e) = self.save_state() {
            tracing::warn!("persist after folder-owner stamp: {e}");
        }
    }

    /// The folder owner recorded for `channel_id`
    /// ([`Self::mark_folder_channel_owner`]), `None` for an unstamped (chat,
    /// scheduling, or not-yet-bound) channel — which keeps today's open commit
    /// processing.
    pub fn folder_channel_owner(&self, channel_id: &ChannelId) -> Option<ActorId> {
        let values = self.provider.storage().values.read().unwrap();
        let v = values.get(&folder_channel_owner_key(channel_id))?;
        <[u8; 32]>::try_from(v.as_slice()).ok().map(ActorId)
    }

    /// Re-point every folder-owner marker naming one of `predecessors` to
    /// **this engine's own identity** — the successor's own-seat half of
    /// *The marker follows the owner's verified succession* (`federation.md`
    /// § Cross-nest shared folders + channel append): an identity that
    /// succeeded from `predecessors` now owns every channel they owned, and a
    /// marker still naming the retired key would refuse its own roster
    /// commits and let that key keep the declassification anchor. Callers
    /// supply only predecessors **this device attested** (the account
    /// registry's `attested_predecessor_actor_ids` — possession of their seeds,
    /// never a served field); the sweep calls it over the identity it just
    /// retired. Markers naming anyone else are untouched. Returns how many
    /// moved; persists the provider snapshot on native when any did.
    pub fn restamp_folder_owner_markers(&self, predecessors: &[ActorId]) -> usize {
        let successor = self.identity_actor_id();
        let moved = {
            let mut values = self.provider.storage().values.write().unwrap();
            let keys: Vec<Vec<u8>> = values
                .iter()
                .filter(|(key, owner)| {
                    key.starts_with(FOLDER_CHANNEL_OWNER_PREFIX)
                        && predecessors
                            .iter()
                            .any(|p| p.0.as_slice() == owner.as_slice())
                })
                .map(|(key, _)| key.clone())
                .collect();
            for key in &keys {
                values.insert(key.clone(), successor.0.to_vec());
            }
            keys.len()
        };
        #[cfg(feature = "native")]
        if moved > 0
            && let Err(e) = self.save_state()
        {
            tracing::warn!("persist after folder-owner re-stamp: {e}");
        }
        moved
    }

    /// Rest `statement` (canonical `SignedIdentitySuccession` bytes, opaque
    /// here) in `channel_id`'s at-rest folder park
    /// ([`PARKED_FOLDER_SUCCESSION_PREFIX`]), replacing any earlier one.
    /// Persists the provider snapshot on native — the park exists to survive
    /// a relaunch.
    pub fn park_folder_succession(&self, channel_id: &ChannelId, statement: &[u8]) {
        {
            let mut values = self.provider.storage().values.write().unwrap();
            values.insert(parked_folder_succession_key(channel_id), statement.to_vec());
        }
        #[cfg(feature = "native")]
        if let Err(e) = self.save_state() {
            tracing::warn!("persist after resting a folder succession statement: {e}");
        }
    }

    /// The statement resting in `channel_id`'s at-rest folder park, if any.
    pub fn parked_folder_succession(&self, channel_id: &ChannelId) -> Option<Vec<u8>> {
        let values = self.provider.storage().values.read().unwrap();
        values
            .get(&parked_folder_succession_key(channel_id))
            .cloned()
    }

    /// Every at-rest folder park this engine holds, as `(channel, statement)`.
    pub fn parked_folder_successions(&self) -> Vec<(ChannelId, Vec<u8>)> {
        let values = self.provider.storage().values.read().unwrap();
        values
            .iter()
            .filter_map(|(key, statement)| {
                let id = key.strip_prefix(PARKED_FOLDER_SUCCESSION_PREFIX)?;
                let id = <[u8; 32]>::try_from(id).ok()?;
                Some((ChannelId(id), statement.clone()))
            })
            .collect()
    }

    /// Forget `channel_id`'s at-rest folder park. Returns whether one was
    /// resting; persists on native only then.
    pub fn forget_parked_folder_succession(&self, channel_id: &ChannelId) -> bool {
        let removed = self.drop_parked_folder_succession(channel_id);
        #[cfg(feature = "native")]
        if removed && let Err(e) = self.save_state() {
            tracing::warn!("persist after forgetting a folder succession statement: {e}");
        }
        removed
    }

    /// The in-memory half of [`Self::forget_parked_folder_succession`].
    fn drop_parked_folder_succession(&self, channel_id: &ChannelId) -> bool {
        let mut values = self.provider.storage().values.write().unwrap();
        values
            .remove(&parked_folder_succession_key(channel_id))
            .is_some()
    }

    /// The MLS-authenticated sender of the Welcome this device joined
    /// `channel_id` from (see [`WELCOME_SENDER_PREFIX`]); `None` for a group
    /// this device created itself or joined before the record existed.
    pub fn welcome_sender(&self, channel_id: &ChannelId) -> Option<ActorId> {
        let values = self.provider.storage().values.read().unwrap();
        let v = values.get(&welcome_sender_key(channel_id))?;
        <[u8; 32]>::try_from(v.as_slice()).ok().map(ActorId)
    }

    /// Spend this device's **history admission** for `channel_id`
    /// ([`HISTORY_ADMISSION_PREFIX`]) on a message from `sender` sealed in
    /// `epoch` — both as [`Self::decrypt_authenticated`] reported them, never
    /// as a payload claims them. `true` exactly once per join: when `sender`
    /// is the account whose Welcome this device joined from and `epoch` is the
    /// one it joined at. The record is removed on that answer, so a second
    /// slice from the same inviter is `false`, as is anything on a channel
    /// this device created or joined before the record existed.
    ///
    /// A mismatch spends nothing — otherwise any member could burn a
    /// newcomer's admission by posting first. Account-level on purpose: the
    /// Welcome's sender is recorded as an `ActorId`, and which of the
    /// inviter's devices committed the Add is not something a joiner holds.
    /// Persists on native, so a relaunch cannot re-open a spent admission.
    pub fn take_history_admission(
        &self,
        channel_id: &ChannelId,
        sender: &ActorId,
        epoch: u64,
    ) -> bool {
        if self.welcome_sender(channel_id).as_ref() != Some(sender) {
            return false;
        }
        {
            let mut values = self.provider.storage().values.write().unwrap();
            let key = history_admission_key(channel_id);
            let joined_at = values
                .get(&key)
                .and_then(|v| <[u8; 8]>::try_from(v.as_slice()).ok())
                .map(u64::from_be_bytes);
            if joined_at != Some(epoch) {
                return false;
            }
            values.remove(&key);
        }
        #[cfg(feature = "native")]
        if let Err(e) = self.save_state() {
            tracing::warn!("persist after spending the history admission: {e}");
        }
        true
    }

    /// Whether `(commit_hash, at this group epoch)` sits in `channel_id`'s
    /// policy-refused memo ([`POLICY_REFUSED_COMMIT_PREFIX`]) — the pre-decrypt
    /// replay answer. Prunes entries at epochs below `group_epoch` on touch.
    fn policy_refused_commit_memo_hit(
        &self,
        channel_id: &ChannelId,
        commit_hash: &[u8; 32],
        group_epoch: u64,
    ) -> bool {
        let key = policy_refused_commit_key(channel_id);
        let mut values = self.provider.storage().values.write().unwrap();
        let Some(v) = values.get(&key) else {
            return false;
        };
        let mut kept = Vec::with_capacity(v.len());
        let mut hit = false;
        for entry in v.as_chunks::<40>().0 {
            let epoch = u64::from_be_bytes(entry[32..40].try_into().unwrap());
            if epoch < group_epoch {
                continue; // pruned: openMLS's epoch precheck skips these pre-decrypt
            }
            if entry[..32] == *commit_hash.as_slice() && epoch == group_epoch {
                hit = true;
            }
            kept.extend_from_slice(entry);
        }
        if kept.is_empty() {
            values.remove(&key);
        } else if kept.len() != v.len() {
            values.insert(key, kept);
        }
        hit
    }

    /// Record a policy-refused commit's identity in `channel_id`'s durable memo
    /// (see [`POLICY_REFUSED_COMMIT_PREFIX`] for why, and the cap). Persists
    /// the provider snapshot on native — the memo must survive a relaunch to do
    /// its job.
    fn record_policy_refused_commit(
        &self,
        channel_id: &ChannelId,
        commit_hash: [u8; 32],
        group_epoch: u64,
    ) {
        let key = policy_refused_commit_key(channel_id);
        {
            let mut values = self.provider.storage().values.write().unwrap();
            let mut v = values.get(&key).cloned().unwrap_or_default();
            // Prune below-epoch entries; cap by dropping oldest.
            let mut kept: Vec<u8> = Vec::with_capacity(v.len() + 40);
            for entry in v.as_chunks::<40>().0 {
                let epoch = u64::from_be_bytes(entry[32..40].try_into().unwrap());
                if epoch >= group_epoch {
                    kept.extend_from_slice(entry);
                }
            }
            v = kept;
            v.extend_from_slice(&commit_hash);
            v.extend_from_slice(&group_epoch.to_be_bytes());
            while v.len() / 40 > POLICY_REFUSED_COMMIT_CAP {
                v.drain(..40);
            }
            values.insert(key, v);
        }
        #[cfg(feature = "native")]
        if let Err(e) = self.save_state() {
            tracing::warn!("persist after policy-refused-commit memo: {e}");
        }
    }

    /// Discard the group's own pending commit — the **clear** leg of the rebase,
    /// run on a `fauna.conversations.channel.stale` rejection. The group returns
    /// to its pre-commit epoch; the caller then processes the intervening records
    /// and rebuilds the commit. Skipped sender-chain generations are tolerated by
    /// openmls receivers (design constraint 2). A safe no-op when no commit is
    /// pending (openmls returns early on an `Operational` group), so the rebase
    /// loop may call it unconditionally.
    pub fn clear_pending_commit(&self, channel_id: &ChannelId) -> Result<()> {
        self.ensure_live()?;
        {
            let mut groups = self.groups.lock().expect("lock poisoned");
            let group = groups
                .get_mut(channel_id)
                .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
            group
                .clear_pending_commit(self.provider.storage())
                .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        }
        self.forget_pending_commit_hash(channel_id);
        Ok(())
    }

    /// Build the MLS credential and signer from the actor's actual Ed25519 key.
    ///
    /// This binds the MLS identity to the Fauna ActorId — the MLS signer
    /// IS the actor's signing key, so group members can verify that leaves
    /// belong to the claimed actor.
    fn build_credential_and_signer(
        identity: &ActorKeypair,
        provider: &OpenMlsRustCrypto,
    ) -> Result<(CredentialWithKey, SignatureKeyPair)> {
        let actor_id = identity.actor_id();
        let credential = BasicCredential::new(actor_id.0.to_vec());

        // Use the actor's actual Ed25519 key as the MLS signer.
        let private_bytes = identity.signing_key().to_bytes().to_vec();
        let public_bytes = identity.verifying_key().to_bytes().to_vec();
        let signer =
            SignatureKeyPair::from_raw(SignatureScheme::ED25519, private_bytes, public_bytes);

        // Store the signer in the provider's key store so OpenMLS can find it.
        signer
            .store(provider.storage())
            .map_err(|e| MlsError::Storage(format!("{e:?}")))?;

        let credential_with_key = CredentialWithKey {
            credential: credential.into(),
            signature_key: signer.public().into(),
        };

        Ok((credential_with_key, signer))
    }

    /// Process an incoming commit message for a group.
    ///
    /// This is needed so other group members can advance their state after
    /// an add/remove operation.
    ///
    /// The error taxonomy is load-bearing for inbound-cursor safety
    /// (`devices.md` § Cross-device MLS group-state sync, Rule 2 — see
    /// [`MlsError::InvalidCommit`]): [`MlsError::InvalidCommit`] means *no
    /// member can ever apply these bytes* (safe to advance past), while
    /// [`MlsError::OpenMls`]/[`MlsError::Storage`]/[`MlsError::ChannelNotFound`]
    /// mean *this device* failed on a commit the group may have incorporated
    /// (a cursor must stop before it).
    ///
    /// **The split is per-variant, never per-class**: openMLS validates the epoch first (`validate_framing` →
    /// `WrongEpoch`, both directions, classified below), so anything reaching
    /// deeper validation is judged at this device's current epoch — but *not*
    /// everything judged there is judged on shared state. Signature, tag,
    /// membership, and proposal checks run against the group's canonical
    /// public tree and epoch secrets (deterministic across members ⇒
    /// intrinsically invalid), while **staging a commit's update path needs
    /// the receiver's own PRIVATE epoch decryption keypairs** — device-local
    /// state a torn snapshot, a replica restore, or a storage read error can
    /// lose with the bytes and the shared tree intact. openMLS launders those
    /// local absences into `StageCommitError::{MissingDecryptionKey,
    /// UpdatePathError, …}` and provider/library failures into `LibraryError`s
    /// at several nesting depths, so each variant is classified explicitly
    /// below, and every ambiguous or unknown variant rounds to *local* (Rule
    /// 2's safe side — a classifier whose two answers have asymmetric blast
    /// radius must never reach the destructive answer through a catch-all).
    pub fn process_commit(&self, channel_id: &ChannelId, commit_bytes: &[u8]) -> Result<()> {
        self.ensure_live()?;
        let mls_msg_in = MlsMessageIn::tls_deserialize_exact(commit_bytes)
            .map_err(|e| MlsError::InvalidCommit(format!("not an MLS message: {e:?}")))?;

        let mut groups = self.groups.lock().expect("lock poisoned");
        let group = groups
            .get_mut(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;

        // A commit whose bytes already consumed this device's ratchet state
        // without merging (a stage-local or merge failure) can never be
        // reprocessed in this process — without this memo the retry would
        // misreport as a validation failure and be skipped as invalid. Keep
        // reporting the same local failure instead; a stale entry (the group
        // has since advanced, e.g. a replica resync healed the channel) is
        // dropped so the record classifies normally (`PastEpochCommit`).
        let group_epoch = group.epoch().as_u64();
        let commit_hash = *blake3::hash(commit_bytes).as_bytes();
        if let Some((hash, at_epoch)) = self.merge_failed_commit_memo(channel_id) {
            if at_epoch != group_epoch {
                self.forget_merge_failed_commit(channel_id);
            } else if hash == commit_hash {
                return Err(MlsError::OpenMls(
                    "commit previously consumed local ratchet state without merging; \
                     un-reprocessable over this provider state — stalled until a \
                     replica resync (or a relaunch onto a pre-consumption snapshot) \
                     heals the channel"
                        .into(),
                ));
            }
        }

        // Pre-decrypt replay answer for a policy-refused commit (see
        // [`POLICY_REFUSED_COMMIT_PREFIX`]): the original refusal consumed the
        // committer's sender-ratchet generation, so re-decrypting the same
        // bytes would misclassify as a local `SecretReuseError` stall. The
        // memo repeats the deterministic verdict instead, before any
        // consumption.
        if self.policy_refused_commit_memo_hit(channel_id, &commit_hash, group_epoch) {
            // No `forget_merge_failed_commit` here: this runs before the memo
            // is armed for these bytes, so a standing merge-failed memo (if
            // any) belongs to a different commit's genuine stall.
            return Err(MlsError::PolicyRefusedCommit {
                reason: "previously refused by the folder commit policy (durable memo)".into(),
            });
        }

        let protocol_msg: ProtocolMessage = mls_msg_in.try_into_protocol_message().map_err(
            |e| MlsError::InvalidCommit(format!("not a protocol message (Welcome/GroupInfo/KeyPackage on the commit rail): {e:?}")),
        )?;

        // The commit's epoch from the unprotected header, and the group's own epoch —
        // both read before the message is consumed / the group is mutably borrowed.
        // `msg_epoch` is carried by the typed `OwnLeafCommit` error for the resync
        // path's crash-window pending-merge equality check, and the pair classifies
        // openMLS's direction-less `WrongEpoch` below.
        let msg_epoch = protocol_msg.epoch().as_u64();

        // Arm the memo ACROSS the consumption window:
        // PrivateMessage decryption consumes the sender-ratchet generation the
        // moment `process_message` begins — before any validation, staging, or
        // merge verdict exists — and openMLS persists that mutated secret tree
        // through the provider as part of processing.
        // The arming point is the same either way: a
        // *successfully-decrypted* message still consumes its generation before
        // the merge verdict exists. Arming only at merge failure left
        // every stage failure consumed-but-unmemoized: once stage-local
        // failures stall (the per-variant split below), an unmemoized retry
        // would re-decrypt the bytes, hit `SecretReuseError`/
        // `GenerationOutOfBound`, and misclassify as intrinsically invalid —
        // re-opening the loss one pass later. Armed here, settled at every
        // exit below: kept on a post-consumption local failure, forgotten on
        // success and on every verdict that permits no retry. Over-
        // approximation is the safe side — a memo for bytes that never
        // consumed at worst stalls the channel until a resync heals it.
        self.record_merge_failed_commit(channel_id, commit_hash, group_epoch);

        let processed = match group.process_message(&self.provider, protocol_msg) {
            Ok(p) => p,
            Err(e) => {
                // Classify to `(error, keep_memo)`. `keep_memo` is true iff the
                // failure is local AND the bytes may have consumed ratchet
                // state (at-or-after decrypt) — a kept memo makes the retry
                // repeat this verdict instead of misclassifying; a dropped one
                // lets a genuinely transient local failure heal on retry.
                let (err, keep_memo) = match e {
                    ProcessMessageError::ValidationError(v) => match v {
                        // (An own-leaf message is not an error since openMLS
                        // 0.9: it arrives as `Ok` with
                        // `ProcessedMessageContent::OwnPrivateMessage`, handled
                        // in the content match below.)
                        // openMLS reports an epoch mismatch in EITHER direction
                        // as `WrongEpoch`; the two are not alike. Behind the
                        // group ⇒ our own merged commit coming back around the
                        // poll, or a replayed foreign commit — quiet skip.
                        // Ahead of it ⇒ we never applied the bridging commit
                        // and can never catch up: a durable strand, loud.
                        ValidationError::WrongEpoch => {
                            if msg_epoch > group_epoch {
                                (MlsError::FutureEpochCommit { epoch: msg_epoch }, false)
                            } else {
                                (MlsError::PastEpochCommit, false)
                            }
                        }
                        // A library-internal error inside validation is a
                        // *local* failure, not a verdict on the bytes — and it
                        // may sit before or after the ratchet consumption, so
                        // the memo stays (the safe over-approximation).
                        ValidationError::LibraryError(e) => {
                            (MlsError::OpenMls(format!("{e:?}")), true)
                        }
                        // Decrypt failures split one nesting level down — see
                        // `classify_decrypt_error` (extracted so the mapping is
                        // unit-testable without the unforgeable-in-test attack
                        // that produces a `SecretReuseError` at runtime).
                        ValidationError::UnableToDecrypt(d) => classify_decrypt_error(d),
                        // Every remaining validation failure is computed on
                        // state every honest member shares at this epoch —
                        // the bytes, the public tree, the epoch's context and
                        // membership/confirmation keys — so the verdict is
                        // deterministic across members: nobody applies these
                        // bytes and the group never advances past them.
                        // Enumerated exhaustively (no catch-all): a future
                        // openMLS variant must be classified consciously, not
                        // rounded to the destructive answer.
                        v @ (ValidationError::WrongGroupId
                        | ValidationError::NotACommit
                        | ValidationError::NotAnExternalAddProposal
                        | ValidationError::NoPath
                        | ValidationError::UnencryptedApplicationMessage
                        | ValidationError::UnknownMember
                        | ValidationError::MissingMembershipTag
                        | ValidationError::InvalidMembershipTag
                        | ValidationError::MissingConfirmationTag
                        | ValidationError::WrongWireFormat
                        | ValidationError::InvalidSignature
                        | ValidationError::NonMemberApplicationMessage
                        | ValidationError::NoPastEpochData
                        | ValidationError::UnauthorizedExternalSender
                        | ValidationError::NoExternalSendersExtension
                        | ValidationError::KeyPackageVerifyError(_)
                        | ValidationError::UpdatePathError(_)
                        | ValidationError::InvalidLeafNodeSignature
                        | ValidationError::InvalidLeafNodeSourceType
                        | ValidationError::InvalidSenderType
                        | ValidationError::CommitterIncludedOwnUpdate
                        | ValidationError::InvalidAddProposalCiphersuite
                        | ValidationError::ExternalCommitValidation(_)
                        | ValidationError::InvalidExtension(_)) => {
                            (MlsError::InvalidCommit(format!("{v:?}")), false)
                        }
                    },
                    // Staging failures — strictly post-decrypt, so the
                    // generation is consumed on every arm here — split by what
                    // the verdict was computed ON:
                    ProcessMessageError::InvalidCommit(e) => match e {
                        // A commit from our own leaf, carrying an UpdatePath,
                        // that is not our pending commit (openMLS 0.9 raises
                        // this for an own-leaf commit that reaches staging, e.g.
                        // in a PublicMessage). Every device of this identity
                        // shares the one leaf, so this is ANOTHER device's
                        // commit — the same cross-device resync signal as the
                        // `OwnPrivateMessage` arm below. A PublicMessage
                        // consumes no ratchet generation: memo dropped.
                        StageCommitError::OwnCommitMismatch => {
                            (MlsError::OwnLeafCommit { epoch: msg_epoch }, false)
                        }
                        // Judged on the RECEIVER'S OWN private key/secret
                        // material (or a provider/library failure) — an
                        // attacker cannot make these fail on every member,
                        // and the demonstrated production shapes (torn
                        // snapshot, replica restore, storage read error)
                        // fail on exactly one. Local ⇒ stall, memo kept.
                        e @ (StageCommitError::LibraryError(_)
                        | StageCommitError::OwnKeyNotFound
                        | StageCommitError::PskError(_)
                        | StageCommitError::UpdatePathError(_)
                        | StageCommitError::VerifiedUpdatePathError(_)
                        | StageCommitError::MissingDecryptionKey) => (
                            MlsError::OpenMls(format!("stage commit failed locally: {e:?}")),
                            true,
                        ),
                        // Judged on the bytes + the shared public tree /
                        // epoch context — deterministic across members.
                        // (`MissingProposal` is deterministic for Fauna:
                        // no code path stores or sends bare proposals, so a
                        // by-reference commit is unresolvable for every
                        // member equally — review-verified.) Enumerated
                        // exhaustively, same rationale as above.
                        e @ (StageCommitError::EpochMismatch
                        | StageCommitError::WrongPlaintextContentType
                        | StageCommitError::PathLeafNodeVerificationFailure
                        | StageCommitError::RequiredPathNotFound
                        | StageCommitError::ConfirmationTagMissing
                        | StageCommitError::ConfirmationTagMismatch
                        | StageCommitError::AttemptedSelfRemoval
                        | StageCommitError::MissingProposal
                        | StageCommitError::InconsistentSenderIndex
                        | StageCommitError::SenderTypeExternal
                        | StageCommitError::SenderTypeNewMemberProposal
                        | StageCommitError::TooManyNewMembers
                        | StageCommitError::ProposalValidationError(_)
                        | StageCommitError::ExternalCommitValidation(_)
                        | StageCommitError::GroupContextExtensionsProposalValidationError(_)
                        | StageCommitError::LeafNodeValidation(_)
                        | StageCommitError::DuplicatePskId(_)) => {
                            (MlsError::InvalidCommit(format!("{e:?}")), false)
                        }
                    },
                    // Wire-format policy and external-sender rejections are
                    // deterministic on the bytes + immutable group policy,
                    // and pre-decrypt.
                    e @ (ProcessMessageError::IncompatibleWireFormat
                    | ProcessMessageError::UnauthorizedExternalApplicationMessage
                    | ProcessMessageError::UnauthorizedExternalCommitMessage
                    | ProcessMessageError::UnsupportedProposalType) => {
                        (MlsError::InvalidCommit(format!("{e:?}")), false)
                    }
                    // Library and storage errors are local, at an unknowable
                    // point relative to the consumption — memo kept (openMLS
                    // persists the mutated secret tree through the provider
                    // mid-decrypt, so a StorageError can be post-consumption).
                    e @ (ProcessMessageError::LibraryError(_)
                    | ProcessMessageError::StorageError(_)) => {
                        (MlsError::OpenMls(format!("{e:?}")), true)
                    }
                    // Group-state errors (e.g. evicted — later records are
                    // genuinely not ours anymore) are local and pre-decrypt:
                    // no memo, so a transient shape may heal on retry. The
                    // rail rounds local toward the safe side (Rule 2: stall).
                    other => (MlsError::OpenMls(format!("{other:?}")), false),
                };
                if !keep_memo {
                    self.forget_merge_failed_commit(channel_id);
                }
                return Err(err);
            }
        };

        // The committer's MLS-authenticated identity (the leaf credential the
        // decrypt bound the message to) — read before `into_content()` consumes
        // the wrapper; the folder commit policy below compares it to the
        // channel's recorded owner.
        let committer_identity: Option<[u8; 32]> =
            <[u8; 32]>::try_from(processed.credential().serialized_content()).ok();

        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged_commit) => {
                // MLS-2: validate every leaf this commit introduces or updates
                // BEFORE merging — the committer's own update-path leaf, any
                // Add-proposal KeyPackage leaves, and any Update-proposal leaves
                // (the exact set openmls would re-credential-verify). Reject a
                // forged-credential leaf instead of admitting it; an honest
                // receiver must not advance into a poisoned roster even if the
                // committer ran a patched client. A reject is deterministic on
                // the leaves in the bytes (every honest member rejects the
                // same forgery — the rail skips it), so the memo is dropped.
                let leaf_check = (|| -> Result<()> {
                    if let Some(leaf) = staged_commit.update_path_leaf_node() {
                        validate_leaf_binding(leaf)?;
                    }
                    for add in staged_commit.add_proposals() {
                        validate_leaf_binding(add.add_proposal().key_package().leaf_node())?;
                    }
                    for upd in staged_commit.update_proposals() {
                        validate_leaf_binding(upd.update_proposal().leaf_node())?;
                    }
                    Ok(())
                })();
                if let Err(e) = leaf_check {
                    self.forget_merge_failed_commit(channel_id);
                    return Err(e);
                }

                // Folder commit policy (owner-managed roster —
                // `federation.md` § Cross-nest shared folders + channel
                // append): on a channel stamped with a folder owner
                // ([`Self::mark_folder_channel_owner`]), a commit carrying ANY
                // proposal (Add/Remove/Update/PSK/extensions — a roster or
                // group-state change) merges only when its authenticated
                // committer IS the owner. A bare self-`Update` commit (no
                // proposals — the device-owned-epoch takeover every member's
                // device legitimately posts before its first application send,
                // `devices.md` § Cross-device MLS group-state sync) merges
                // from any member. The nest cannot enforce this — commits are
                // PrivateMessage ciphertext to it — so every honest member
                // enforces it here, deterministically (same bytes + same
                // durable owner marker → same verdict; the rail skips it like
                // an intrinsically invalid commit). The refusal is recorded in
                // the durable policy-refused memo because the decrypt above
                // already consumed the committer's sender-ratchet generation —
                // see [`POLICY_REFUSED_COMMIT_PREFIX`].
                if let Some(owner) = self.folder_channel_owner(channel_id) {
                    let carries_proposals = staged_commit.queued_proposals().next().is_some();
                    if carries_proposals && committer_identity != Some(owner.0) {
                        self.record_policy_refused_commit(channel_id, commit_hash, group_epoch);
                        self.forget_merge_failed_commit(channel_id);
                        return Err(MlsError::PolicyRefusedCommit {
                            reason: format!(
                                "proposal-carrying commit from a non-owner member \
                                 (committer {}, owner {}) on an owner-managed folder channel",
                                committer_identity
                                    .map(hex::encode)
                                    .unwrap_or_else(|| "<unreadable>".into()),
                                hex::encode(owner.0)
                            ),
                        });
                    }
                }

                // Room commit policy (`crate::room_policy`): on a channel whose
                // group context carries a room policy, the roles table decides
                // whether THIS committer may do what the staged commit does —
                // the same verdict every honest member computes on the same
                // agreed bytes, refused through the same durable memo as the
                // folder policy above (the decrypt has already consumed the
                // committer's sender-ratchet generation).
                if let Some(current) = room_policy_in(group.extensions()) {
                    let verdict = match (current, committer_identity) {
                        (Err(e), _) => CommitVerdict::Refuse(format!(
                            "the room's own policy extension is invalid: {e}"
                        )),
                        (Ok(_), None) => CommitVerdict::Refuse(
                            "the committer's credential is not a fauna actor id".into(),
                        ),
                        (Ok(current), Some(committer)) => {
                            let facts =
                                staged_commit_facts(channel_id, group, &staged_commit, committer);
                            judge_commit(&current, &facts)
                        }
                    };
                    if let CommitVerdict::Refuse(reason) = verdict {
                        self.record_policy_refused_commit(channel_id, commit_hash, group_epoch);
                        self.forget_merge_failed_commit(channel_id);
                        return Err(MlsError::PolicyRefusedCommit {
                            reason: format!("room policy: {reason}"),
                        });
                    }
                }

                // Cache the current epoch's blob key before advancing.
                #[cfg(feature = "native")]
                {
                    let epoch = group.epoch().as_u64();
                    if let Ok(secret) =
                        group.export_secret(self.provider.crypto(), "fauna.blob.v1", &[], 32)
                    {
                        let mut key = [0u8; 32];
                        key.copy_from_slice(&secret);
                        let _ = self.storage.put_blob_epoch_key(&channel_id.0, epoch, &key);
                    }
                }
                // ...and its room-post secret, so a room-restricted post sealed
                // at this epoch still opens here after the group moves on.
                self.remember_room_post_secret(channel_id, group);

                group
                    .merge_staged_commit(&self.provider, *staged_commit)
                    .map_err(|e| {
                        // A validated commit whose LOCAL merge failed: the
                        // group advanced without us. The memo armed above
                        // stays — the decrypt consumed the sender-ratchet
                        // generation, so only it keeps the retry classified
                        // as the local failure it is (see
                        // `MERGE_FAILED_COMMIT_PREFIX`).
                        MlsError::OpenMls(format!("merge_staged_commit failed: {e:?}"))
                    })?;
                self.forget_merge_failed_commit(channel_id);
                Ok(())
            }
            // An own-leaf message on a decryptable (current) epoch: our own
            // already-merged commit would be `WrongEpoch` above, so this means
            // ANOTHER device of this identity advanced the group — the
            // cross-device resync signal (design §3 "own-leaf foreign commit";
            // `docs/goal/behavior/devices.md`). openMLS recognises the sender
            // from the sender data and returns before touching any ratchet —
            // nothing consumed, so the memo is dropped. Must never fall into
            // the non-commit arm below: skipping it would lose the resync.
            ProcessedMessageContent::OwnPrivateMessage => {
                self.forget_merge_failed_commit(channel_id);
                Err(MlsError::OwnLeafCommit { epoch: msg_epoch })
            }
            // Decrypted fine but the content is not a commit (an application
            // message or bare proposal on the commit rail) — deterministic on
            // the bytes: every member sees the same non-commit.
            other => {
                self.forget_merge_failed_commit(channel_id);
                Err(MlsError::InvalidCommit(format!(
                    "expected StagedCommitMessage, got: {other:?}"
                )))
            }
        }
    }

    /// Persist an active group mapping and save the provider snapshot.
    #[cfg(feature = "native")]
    fn persist_group(&self, channel_id: &ChannelId, group: &MlsGroup) {
        let mls_group_id = group.group_id().as_slice().to_vec();
        if let Err(e) = self.storage.put_active_group(&channel_id.0, &mls_group_id) {
            tracing::warn!("failed to persist active group: {e}");
        }
        if let Err(e) = self.save_state() {
            tracing::warn!("failed to save provider state: {e}");
        }
    }

    /// Export and save the provider's in-memory storage to SQLite.
    #[cfg(feature = "native")]
    pub fn save_state(&self) -> Result<()> {
        let bytes = encode_provider_snapshot(&self.export_provider_storage())?;
        self.storage
            .save_provider_snapshot(&bytes)
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    /// Hand this engine's **conversations-engine role** to a successor over the
    /// same `mls_state.db`: flush the provider snapshot, then release the role
    /// lock and refuse every later statement
    /// ([`crate::storage::MlsStateRetired`]).
    ///
    /// Call it on the engine being replaced, **before** constructing its
    /// successor — the whole point is that the successor's
    /// `SqliteStorage::open` must find the lock free. The shared native factory
    /// (`fauna_ffi::FfiNestClient::conversations_session*`) does this for every
    /// UniFFI app through `ConversationsManager::retire_conversations_engine`,
    /// so no app shell has to remember it.
    ///
    /// Flushing first is not optional: the provider's in-memory storage is
    /// written back on group mutations, and a hand-over that dropped the role
    /// without a final `save_state` would strand whatever the last mutation
    /// left unsaved in an engine that can no longer write it. A flush failure is
    /// logged, never propagated — the role must be released either way, or the
    /// successor is refused and the account loses conversations entirely.
    /// Idempotent.
    #[cfg(feature = "native")]
    pub fn retire(&self) {
        if let Err(e) = self.save_state() {
            tracing::warn!("flushing provider state before retiring the MLS engine failed: {e}");
        }
        self.storage.retire();
        // ⚠ LAST, and for the same reason the storage poison is last: the flush
        // above is a group-touching write, so a quiesce that flipped before it
        // would refuse the very save the hand-over depends on and strand
        // whatever the final mutation left unsaved.
        self.retired.store(true, Ordering::SeqCst);
    }

    /// Whether this engine's role has been handed over ([`Self::retire`]).
    /// Callers that walk a message log must check this **before** advancing a
    /// cursor — see [`MlsError::Retired`].
    pub fn is_retired(&self) -> bool {
        self.retired.load(Ordering::SeqCst)
    }

    /// Refuse [`MlsError::Retired`] once the role has been handed over.
    ///
    /// Guards every method that mutates group state or emits bytes another
    /// member will act on. Read-only accessors are deliberately NOT guarded:
    /// a retired engine may still be asked what it knew, and the teardown paths
    /// that ask are the reason the hand-over is graceful rather than a drop.
    fn ensure_live(&self) -> Result<()> {
        if self.is_retired() {
            return Err(MlsError::Retired);
        }
        Ok(())
    }

    /// List all channel IDs for groups this engine belongs to.
    pub fn list_groups(&self) -> Vec<ChannelId> {
        let groups = self.groups.lock().unwrap();
        groups.keys().copied().collect()
    }

    /// List all (ChannelId, raw MLS GroupId bytes) pairs.
    ///
    /// Needed for state export: the ChannelId is a hash of the GroupId,
    /// so the original GroupId must be stored alongside it for import.
    pub fn list_groups_with_raw_ids(&self) -> Vec<(ChannelId, Vec<u8>)> {
        let groups = self.groups.lock().unwrap();
        groups
            .iter()
            .map(|(cid, group)| (*cid, group.group_id().as_slice().to_vec()))
            .collect()
    }

    /// Check if this engine has a group for the given channel.
    pub fn has_group(&self, channel_id: &ChannelId) -> bool {
        let groups = self.groups.lock().unwrap();
        groups.contains_key(channel_id)
    }

    /// Whether this device still **holds its seat** in `channel_id`'s group:
    /// the group is loaded here *and* OpenMLS still calls it active. A device
    /// that has processed the commit removing it keeps the group loaded
    /// ([`Self::has_group`] stays true — the state every removed user's snapshot
    /// carries from then on) but the group is **evicted**: it can no longer
    /// export a secret for the post-removal epoch (`UseAfterEviction`), so it
    /// can seal nothing new for the room, while what it kept for earlier epochs
    /// still answers ([`Self::room_post_secret_at`] — a rotated-out member keeps
    /// what it held, `ui/feed.md` ruling 6). `false` for a channel this engine
    /// has no group for.
    ///
    /// This is the one liveness signal a device has about its own membership,
    /// and only for a device that has *seen* its removal: a device that never
    /// processed the commit has nothing to detect, which is why the residue
    /// `ui/feed.md` ruling 5 states cannot be closed from here.
    pub fn is_group_active(&self, channel_id: &ChannelId) -> bool {
        let groups = self.groups.lock().unwrap();
        groups
            .get(channel_id)
            .is_some_and(|group| group.is_active())
    }

    /// Locally **forget** a group — the client-side leave primitive for a shared
    /// folder (`docs/goal/ui/folders.md` § Sharing: a `folder-leave-button`
    /// "to remove yourself"). Drops the group from the engine so
    /// [`Self::has_group`] returns `false` — the B3 member-visible-list join-filter
    /// (`has_group(ChannelId::from_group_id(mls_group_id))`) then hides the set —
    /// and removes the durable `active_groups` row so a restart does **not** reload
    /// it (else `has_group` would flip back true). Idempotent — forgetting an
    /// unknown channel is a no-op.
    ///
    /// Deliberately does **not** purge the group's past-epoch crypto state from the
    /// provider: a voluntary leaver keeps the content generations they already held
    /// ("forward secrecy from yourself is not a meaningful threat",
    /// `docs/goal/architecture/mls-group-key-material.md` § M2). The complementary
    /// nest-side roster self-drop (`fauna.folders.leave`) is a separate step the
    /// caller drives — off the roster, the leaver stops receiving content-key
    /// rotations.
    pub fn forget_group(&self, channel_id: &ChannelId) -> Result<()> {
        // In the quiesce class: it drops the group from `self.groups` and its
        // durable `active_groups` row. Unguarded it half-applied on a retired
        // engine — the in-memory removal landed, then the poisoned store made
        // the durable removal an `Err` — so the refusal is typed and total
        // instead (`account-data-plane.md` § Multi-instance concurrency).
        self.ensure_live()?;
        {
            let mut groups = self.groups.lock().expect("lock poisoned");
            groups.remove(channel_id);
        }
        // A forgotten group carries no pending, so its identity stamp would be a
        // lie a later snapshot could restore.
        self.forget_pending_commit_hash(channel_id);
        // The one per-channel marker that goes with the group: a statement
        // rested for a channel this seat left holds no walk and re-points no
        // marker, and a rejoin must not inherit a hold from the old seat. The
        // other markers stay (never cleared, by their own contracts).
        self.forget_parked_folder_succession(channel_id);
        // Off the last-seen listing too: a rejoin before the next landed flush
        // is a join, and the swap must carry it, not read it as a sibling's
        // deletion.
        self.unlist_replica_channel(channel_id);
        // Native: drop the durable `active_groups` row so a restart's `new()` does
        // not reload the group from SQLite. On wasm the active set *is* the in-memory
        // map (persisted via `list_groups_with_raw_ids` / provider export), so the
        // map drop above is the whole forget.
        #[cfg(feature = "native")]
        self.storage
            .remove_active_group(&channel_id.0)
            .map_err(|e| MlsError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Record that a replica listing naming `channels` **landed from this
    /// device** (its own export, flushed). The groups this engine still holds
    /// join its last-seen listing, the ancestor the provider swap asks against
    /// (`devices.md` § Cross-device MLS group-state sync → *A sibling's
    /// deletion is not a join*). Called by the sync plane's save doors after a
    /// genuine write, never before: a flush that did not land lists nothing,
    /// and recording it would make the next swap drop a join the replica never
    /// saw.
    ///
    /// Only held groups are recorded. The export is taken before the write
    /// lands, and a group forgotten in between must stay off the record, so a
    /// rejoin stays a join. Best-effort durability (a failed native write is
    /// warned): a missing entry errs toward carrying, the pre-record shape,
    /// never toward dropping a join.
    pub fn note_replica_listed(&self, channels: &[ChannelId]) {
        let held: Vec<ChannelId> = {
            let groups = self.groups.lock().expect("lock poisoned");
            channels
                .iter()
                .filter(|c| groups.contains_key(c))
                .copied()
                .collect()
        };
        let mut listed = self.replica_listed.lock().expect("lock poisoned");
        let fresh: Vec<ChannelId> = held.into_iter().filter(|c| listed.insert(*c)).collect();
        #[cfg(feature = "native")]
        if !fresh.is_empty()
            && let Err(e) = self
                .storage
                .add_replica_listed(&fresh.iter().map(|c| c.0).collect::<Vec<_>>())
        {
            tracing::warn!("failed to record the landed replica listing: {e}");
        }
        #[cfg(not(feature = "native"))]
        let _ = fresh;
    }

    /// Take one channel off the last-seen replica listing (a forget).
    fn unlist_replica_channel(&self, channel_id: &ChannelId) {
        let was_listed = self
            .replica_listed
            .lock()
            .expect("lock poisoned")
            .remove(channel_id);
        #[cfg(feature = "native")]
        if was_listed && let Err(e) = self.storage.remove_replica_listed(&channel_id.0) {
            tracing::warn!(
                channel = %channel_id,
                "failed to take a forgotten group off the last-seen replica listing: {e}"
            );
        }
        #[cfg(not(feature = "native"))]
        let _ = was_listed;
    }

    /// Export the provider's in-memory storage as a raw key-value map.
    ///
    /// This captures all OpenMLS state (group trees, epoch secrets, key
    /// packages, etc.) so it can be serialized and persisted externally
    /// (e.g. to IndexedDB in a WASM environment).
    pub fn export_provider_storage(&self) -> HashMap<Vec<u8>, Vec<u8>> {
        let values = self.provider.storage().values.read().unwrap();
        values.clone()
    }

    /// Restore groups from a previously exported provider storage snapshot.
    ///
    /// Populates the provider's in-memory storage with the given key-value
    /// pairs, then attempts to load each group. Each entry in `group_ids`
    /// is a `(ChannelId, raw_mls_group_id_bytes)` pair. Groups that fail
    /// to load are silently skipped.
    ///
    /// ⚠ **`pub(crate)` on purpose — this is the whole-KV swap, and the only
    /// way to reach it is `ProviderReplica::restore_into`, which asks rule
    /// (1)'s seating question first** (`succession-aftermath.md` § Re-key scope
    /// → *What a successor's replica restore may take from a predecessor's*).
    /// A snapshot another identity's engine captured seats this engine as that
    /// identity, so "who may swap the KV" is a security question, not an API
    /// convenience: sealing the primitive is what makes the single door
    /// structural rather than a convention a future call site can miss.
    ///
    /// **A group this engine holds that the snapshot does not list is carried
    /// across the swap — iff it positively seats this identity — and dropped
    /// otherwise** (`devices.md` § Cross-device MLS group-state sync → *A group
    /// the engine holds but the snapshot does not list survives the swap*).
    /// The shape that needs it: a Welcome join persisted to the native store,
    /// a quit inside the autosave debounce, and a relaunch that reloads the
    /// group from SQLite and then restores a snapshot sealed before the join.
    /// Until 2026-09-22 the swap wiped the group's entries while the
    /// insert-only loop below kept it LISTED, so the next snapshot named a
    /// group with no state, `retire()` flushed that KV over the local copy,
    /// and one launch later the join was gone from every store the account
    /// has — with no re-Welcome for a member already in the group. The carry
    /// is the adoption door's shape, not a splice: the snapshot is still taken
    /// **wholesale** (its globals included — the key packages no per-group
    /// attribution owns), and each local-only group is then re-adopted as one
    /// coherent unit — its entries attributed by openMLS's own delete over
    /// the engine's pre-swap bytes plus fauna's own per-channel markers, its
    /// seat asked positively as `ProviderReplica::import_group_into` asks it
    /// (a per-group decision has no sibling group to answer for a blank seat).
    /// A local-only group that is refused — foreign seat, blank seat, will not
    /// load, no entries — is dropped from the map and its native row swept:
    /// the map never again names a group the KV holds no byte of. **Before
    /// any of that, a local-only group this device's last-seen listing names is
    /// dropped the same way.** It is not a join since the snapshot: another
    /// device took it off the listing (a folder leave writes no MLS commit, so
    /// the seat alone cannot tell), and carrying it would re-list it
    /// (`devices.md` → *A sibling's deletion is not a join*; the record is
    /// [`Self::note_replica_listed`]'s, and this swap replaces it with the
    /// listing it adopted). Both
    /// production callers of the door (the launch restore and the own-leaf
    /// resync) get the same rule, since both reach this one function.
    ///
    /// ⚠ Also in the quiesce class, and for the same reason as every other
    /// group-state mutation: swapping the KV under a **retired** engine
    /// re-populates groups that engine may no longer persist a byte of
    /// (`account-data-plane.md` § Multi-instance concurrency). Reachable with
    /// no attacker — a late `restore_and_wire` retry, or the predecessor's
    /// still-draining receive loop calling `resync_provider`.
    pub(crate) fn restore_from_provider_storage(
        &self,
        values: HashMap<Vec<u8>, Vec<u8>>,
        group_ids: &[(ChannelId, Vec<u8>)],
    ) -> Result<()> {
        use crate::state_replica::{ExamineFailure, examine_group_in, scratch_over_map};

        self.ensure_live()?;
        let mut groups = self.groups.lock().expect("lock poisoned");
        let identity = self.identity_actor_id();

        // The engine's own local-only groups, examined BEFORE the swap over a
        // scratch copy of its pre-swap bytes.
        let listed: HashSet<&ChannelId> = group_ids.iter().map(|(cid, _)| cid).collect();
        let mut carried: Vec<CarriedGroup> = Vec::new();
        let mut dropped: Vec<ChannelId> = Vec::new();
        {
            let store = self.provider.storage().values.read().unwrap();
            // The join-or-deletion question comes first, asked against this
            // device's last-seen listing: a local-only group that listing named
            // was taken off by another device (a folder leave writes no MLS
            // commit, so this copy still seats the account), and carrying it
            // would re-list it on this device's next flush.
            let last_seen = self.replica_listed.lock().expect("lock poisoned");
            let local_only: Vec<(ChannelId, Vec<u8>)> = groups
                .iter()
                .filter(|(cid, _)| !listed.contains(cid))
                .filter(|(cid, _)| {
                    if last_seen.contains(*cid) {
                        tracing::info!(
                            channel = %cid,
                            "provider restore: dropping a group this engine holds that the \
                             snapshot does not list — this device saw it listed, so another \
                             device deleted it (a folder leave), not a join since"
                        );
                        dropped.push(**cid);
                        false
                    } else {
                        true
                    }
                })
                .map(|(cid, group)| (*cid, group.group_id().as_slice().to_vec()))
                .collect();
            drop(last_seen);
            if !local_only.is_empty() {
                let scratch = scratch_over_map(&store);
                for (channel_id, raw_group_id) in local_only {
                    match examine_group_in(&scratch, &raw_group_id) {
                        Ok(examined)
                            if examined.seated_as.as_deref() == Some(identity.0.as_slice()) =>
                        {
                            let mut entries = examined.entries;
                            // Fauna's own per-channel markers (chat/scheduling
                            // kind, welcome sender, history admission, …) are
                            // keyed by the raw channel id, which openMLS's
                            // delete never sees; they are the group's state
                            // all the same.
                            entries.extend(
                                store
                                    .iter()
                                    .filter(|(k, _)| is_fauna_channel_key(k, &channel_id))
                                    .map(|(k, v)| (k.clone(), v.clone())),
                            );
                            tracing::info!(
                                channel = %channel_id,
                                "provider restore: carrying a group this engine holds that the \
                                 snapshot does not list (joined since the snapshot was sealed)"
                            );
                            carried.push(CarriedGroup {
                                channel_id,
                                raw_group_id,
                                entries,
                            });
                        }
                        Ok(examined) => {
                            if examined.seated_as.is_some() {
                                tracing::warn!(
                                    channel = %channel_id,
                                    "provider restore: dropping a group this engine holds that \
                                     the snapshot does not list — it seats another identity's \
                                     leaf (rule (1))"
                                );
                            } else {
                                tracing::info!(
                                    channel = %channel_id,
                                    "provider restore: dropping a group this engine holds that \
                                     the snapshot does not list — its own leaf seats nobody (an \
                                     eviction that already landed)"
                                );
                            }
                            dropped.push(channel_id);
                        }
                        Err(failure) => {
                            let why = match failure {
                                ExamineFailure::DoesNotLoad => {
                                    "it does not load out of the engine's own bytes".to_string()
                                }
                                ExamineFailure::ScratchDelete(e) => format!("scratch delete: {e}"),
                                ExamineFailure::NoEntries => {
                                    "attribution established no entry".to_string()
                                }
                            };
                            tracing::warn!(
                                channel = %channel_id,
                                "provider restore: dropping a group this engine holds that the \
                                 snapshot does not list — {why}"
                            );
                            dropped.push(channel_id);
                        }
                    }
                }
            }
        }

        // The swap — wholesale — then the carried groups' entries over it. A
        // collision can only be a stray: the snapshot does not list the group,
        // so any entry of it there is an orphan, and the local copy is the
        // coherent joined state.
        {
            let mut store = self.provider.storage().values.write().unwrap();
            *store = values;
            for group in &carried {
                for (k, v) in &group.entries {
                    store.insert(k.clone(), v.clone());
                }
            }
        }

        // Re-store the signer so OpenMLS can find it after the storage swap.
        let _ = self.signer.store(self.provider.storage());

        // Reload each listed group from the storage provider. The ones that
        // load are the listing this engine adopted: its new last-seen listing.
        let mut adopted_listing: Vec<ChannelId> = Vec::new();
        for (channel_id, raw_group_id) in group_ids {
            let group_id = GroupId::from_slice(raw_group_id);
            match MlsGroup::load(self.provider.storage(), &group_id) {
                Ok(Some(group)) => {
                    groups.insert(*channel_id, group);
                    adopted_listing.push(*channel_id);
                }
                Ok(None) => {
                    // Group not found in storage — skip.
                }
                Err(_) => {
                    // Failed to load — skip.
                }
            }
        }
        // And each carried one; a carried group that will not load after the
        // swap is rolled out again and dropped like a refused one.
        for CarriedGroup {
            channel_id,
            raw_group_id,
            entries,
        } in carried
        {
            match MlsGroup::load(self.provider.storage(), &GroupId::from_slice(&raw_group_id)) {
                Ok(Some(group)) => {
                    groups.insert(channel_id, group);
                }
                other => {
                    tracing::warn!(
                        channel = %channel_id,
                        "provider restore: a carried group does not load after the swap ({}) — \
                         dropped",
                        match other {
                            Ok(None) => "not found after re-insert".to_string(),
                            Err(e) => format!("{e:?}"),
                            Ok(Some(_)) => unreachable!(),
                        }
                    );
                    let mut store = self.provider.storage().values.write().unwrap();
                    for (k, _) in &entries {
                        store.remove(k);
                    }
                    dropped.push(channel_id);
                }
            }
        }
        for channel_id in dropped {
            groups.remove(&channel_id);
            // Native: the durable `active_groups` row goes with it, else the
            // next `new()` reloads a row whose group has no state (and sweeps
            // it there, one warn later than necessary).
            #[cfg(feature = "native")]
            if let Err(e) = self.storage.remove_active_group(&channel_id.0) {
                tracing::warn!(
                    channel = %channel_id,
                    "provider restore: failed to remove the dropped group's active-group row: {e}"
                );
            }
        }
        // The adopted listing replaces the last-seen one. Carried groups stay
        // off it: they are joins this replica has not listed yet, and they join
        // the record when a flush of them lands.
        {
            let mut last_seen = self.replica_listed.lock().expect("lock poisoned");
            *last_seen = adopted_listing.iter().copied().collect();
        }
        #[cfg(feature = "native")]
        if let Err(e) = self
            .storage
            .replace_replica_listed(&adopted_listing.iter().map(|c| c.0).collect::<Vec<_>>())
        {
            tracing::warn!("provider restore: failed to record the adopted replica listing: {e}");
        }
        Ok(())
    }

    /// Insert ONE group's entries into the live store and load the group — the
    /// targeted-import primitive behind `ProviderReplica::import_group_into`,
    /// which is the only door to it (it asks rule (1)'s seating question and
    /// establishes the entry set through openMLS's own delete). `pub(crate)`
    /// for the same reason [`Self::restore_from_provider_storage`] is: what may
    /// enter the KV is a security question, so the primitive stays behind the
    /// door that asks it.
    ///
    /// **Refuses on any overlap.** The engine must hold neither the group nor a
    /// single one of its keys; the import is for a group this engine has never
    /// seen, and an overlap would be exactly the splice the conflict ruling's
    /// rejection 3 forbids. Atomic from the caller's view: a group that will not
    /// load after the insert is rolled out again, entry by entry, and the store
    /// is as it was.
    ///
    /// In the quiesce class like every group-state mutation: a retired engine
    /// refuses (`account-data-plane.md` § Multi-instance concurrency).
    pub(crate) fn adopt_group_entries(
        &self,
        entries: &[(Vec<u8>, Vec<u8>)],
        channel_id: &ChannelId,
        raw_group_id: &[u8],
    ) -> Result<()> {
        self.ensure_live()?;
        let mut groups = self.groups.lock().expect("lock poisoned");
        if groups.contains_key(channel_id) {
            return Err(MlsError::PolicyViolation(format!(
                "adopt {channel_id}: the engine already holds this group"
            )));
        }
        {
            let mut store = self.provider.storage().values.write().unwrap();
            if let Some((k, _)) = entries.iter().find(|(k, _)| store.contains_key(k)) {
                return Err(MlsError::PolicyViolation(format!(
                    "adopt {channel_id}: the engine already holds one of the group's entries \
                     ({} bytes of key) — refusing to splice over live state",
                    k.len()
                )));
            }
            for (k, v) in entries {
                store.insert(k.clone(), v.clone());
            }
        }
        let group_id = GroupId::from_slice(raw_group_id);
        match MlsGroup::load(self.provider.storage(), &group_id) {
            Ok(Some(group)) => {
                #[cfg(feature = "native")]
                self.persist_group(channel_id, &group);
                groups.insert(*channel_id, group);
                drop(groups);
                // Adopted from a listing, so on this device's last-seen one.
                self.note_replica_listed(&[*channel_id]);
                Ok(())
            }
            other => {
                let mut store = self.provider.storage().values.write().unwrap();
                for (k, _) in entries {
                    store.remove(k);
                }
                Err(MlsError::Storage(format!(
                    "adopt {channel_id}: the imported group does not load ({}) — rolled back",
                    match other {
                        Ok(None) => "not found after insert".to_string(),
                        Err(e) => format!("{e:?}"),
                        Ok(Some(_)) => unreachable!(),
                    }
                )))
            }
        }
    }

    /// Encrypt a ChannelMessage and wrap it in an Application envelope, ready to post.
    pub fn encrypt_to_envelope(
        &self,
        channel_id: &ChannelId,
        message: &ChannelMessage,
    ) -> Result<Vec<u8>> {
        let ciphertext = self.encrypt(channel_id, message)?;
        let envelope = ChannelEnvelope::Application(ciphertext);
        fauna_cbor::encode_canonical(&envelope).map_err(|e| MlsError::Encoding(e.to_string()))
    }

    /// Build a one-off MLS *scheduling* delivery for a single recipient: create a
    /// fresh group whose only other member is `recipient_kp_bytes`, and seal
    /// `imip_rfc5322` as the group's first (and only) application message tagged
    /// [`ChannelMessageBody::Scheduling`]. Returns the Welcome the recipient
    /// processes to join, the derived channel id, and the ready-to-post
    /// `ChannelEnvelope::Application` bytes — the three blobs `welcome.deliver` +
    /// `channel.send` carry.
    ///
    /// The signing identity is whatever this engine carries. The CalDAV
    /// auto-schedule **gateway** drives this from an *ephemeral* engine — the MDA
    /// never holds the organizer's Ed25519 secret (CalDAV auth is
    /// password→capability), and that is sound because the recipient ignores
    /// the one-off MLS creator credential: what identifies the deliverer is the
    /// record author the channel's home nest attests for the `channel.send`
    /// that posts the envelope, never anything inside the iMIP
    /// (`caldav-server.md` § Who may mutate an existing event over the inbound
    /// rail). The Fauna
    /// **client** rail drives it from the organizer's own engine. `sender` is
    /// stamped as the app-level sender (the real organizer in both cases); the
    /// receive loop never surfaces it (`caldav-server.md` § Server-side
    /// auto-schedule). Single-sourced so both call sites seal identically.
    pub fn build_scheduling_delivery(
        &self,
        recipient_kp_bytes: &[u8],
        sender: ActorId,
        imip_rfc5322: Vec<u8>,
    ) -> Result<SchedulingDelivery> {
        let kp = self.key_package_from_bytes(recipient_kp_bytes)?;
        let (channel_id, welcome) = self.create_group(&[kp])?;
        // The organizer's engine retains this group, and nothing ever binds it
        // to a thread — without the durable marker it would classify as
        // folder rail (and, worse, as a blocking seat in the cross-group
        // eviction; see CHANNEL_KIND_SCHEDULING_PREFIX). The MDA gateway's
        // ephemeral engine stamps harmlessly.
        self.mark_channel_scheduling(&channel_id);
        let welcome_bytes = welcome
            .to_bytes()
            .map_err(|e| MlsError::Encoding(format!("serialize welcome: {e:?}")))?;
        // A one-off group's first (and only) application message: sequence 1 at
        // epoch 0 (the just-created group's epoch), mirroring the chat rail's
        // first post.
        let message = ChannelMessage {
            sender,
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Scheduling(imip_rfc5322),
            timestamp: Timestamp::now(),
        };
        let app_envelope = self.encrypt_to_envelope(&channel_id, &message)?;
        Ok(SchedulingDelivery {
            welcome_bytes,
            channel_id,
            app_envelope,
        })
    }

    /// Decrypt and check per-sender sequence ordering.
    #[cfg(feature = "native")]
    pub fn decrypt_with_ordering(
        &self,
        channel_id: &ChannelId,
        ciphertext: &[u8],
    ) -> Result<OrderedDecryptResult> {
        let msg = self.decrypt(channel_id, ciphertext)?;
        let sender_bytes = msg.sender.0;
        let last_seq = self
            .storage
            .last_recv_sequence(&channel_id.0, &sender_bytes)
            .map_err(|e| MlsError::Storage(e.to_string()))?;

        if msg.sequence <= last_seq {
            return Ok(OrderedDecryptResult::Duplicate);
        }

        self.storage
            .set_recv_sequence(&channel_id.0, &sender_bytes, msg.sequence)
            .map_err(|e| MlsError::Storage(e.to_string()))?;

        if msg.sequence > last_seq + 1 {
            Ok(OrderedDecryptResult::DeliverWithGap {
                expected_seq: last_seq + 1,
                actual_seq: msg.sequence,
                message: msg,
            })
        } else {
            Ok(OrderedDecryptResult::Deliver(msg))
        }
    }

    /// Encrypt a message body with auto-assigned sequence number and current epoch.
    #[cfg(feature = "native")]
    pub fn encrypt_with_sequence(
        &self,
        channel_id: &ChannelId,
        body: ChannelMessageBody,
    ) -> Result<Vec<u8>> {
        let seq = self
            .storage
            .next_send_sequence(&channel_id.0)
            .map_err(|e| MlsError::Storage(e.to_string()))?;
        let epoch = {
            let groups = self.groups.lock().unwrap();
            let group = groups
                .get(channel_id)
                .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
            group.epoch().as_u64()
        };
        let msg = ChannelMessage {
            sender: self.identity_actor_id(),
            sequence: seq,
            channel_epoch: epoch,
            body,
            timestamp: Timestamp::now(),
        };
        self.encrypt(channel_id, &msg)
    }

    /// Find the leaf index of a member by their ActorId credential.
    ///
    /// Returns `None` if the channel does not exist or the member is not in
    /// the group.
    pub fn find_leaf_by_identity(&self, channel_id: &ChannelId, actor_id: &ActorId) -> Option<u32> {
        self.find_leaves_by_identity(channel_id, actor_id)
            .into_iter()
            .next()
    }

    /// **Every** leaf index whose credential names `actor_id`, ascending.
    ///
    /// ## Today this returns at most one, and that is an invariant from elsewhere
    ///
    /// A fauna credential *is* its leaf signature key — [`validate_leaf_binding`]
    /// enforces that at every admission point, send and receive alike — and
    /// OpenMLS independently refuses a commit seating a signature key already in
    /// the tree (`ProposalValidationError(DuplicateSignatureKey)`, pinned by
    /// `succession::tests`). So a doubly-seated credential is not representable
    /// in a group any fauna client would accept.
    ///
    /// That invariant is real but it lives two layers away from the callers who
    /// depend on it. [`Self::find_leaf_by_identity`]'s first-match answer is
    /// correct *only* while it holds; this method is correct either way. Use it
    /// wherever the question is **eviction** — where a missed second leaf would
    /// mean an identity the caller believes it removed is still reading —
    /// so that the eviction path states its own requirement rather than
    /// inheriting one silently. `succession::commit_remove_old` is that caller
    /// (`docs/goal/behavior/identity-succession.md` § Propagation → *MLS groups*).
    pub fn find_leaves_by_identity(&self, channel_id: &ChannelId, actor_id: &ActorId) -> Vec<u32> {
        let groups = self.groups.lock().unwrap();
        let Some(group) = groups.get(channel_id) else {
            return Vec::new();
        };
        let mut leaves: Vec<u32> = group
            .members()
            .filter(|m| m.credential.serialized_content() == actor_id.0.as_slice())
            .map(|m| m.index.u32())
            .collect();
        leaves.sort_unstable();
        leaves
    }

    /// The identity this engine's **own leaf** carries in a channel's group —
    /// `None` when the channel is unknown here or the own leaf's credential is
    /// not a fauna actor id.
    ///
    /// Normally [`Self::identity_actor_id`], and the cases where it is not are
    /// exactly the ones worth asking about: a provider snapshot captured by
    /// *another* identity's engine restored over this one seats it under that
    /// identity's leaf (a predecessor's, after a succession —
    /// [`crate::state_replica::ProviderReplica::seating_verdict`]
    /// asks the same question of a snapshot before it is restored).
    pub fn own_leaf_identity(&self, channel_id: &ChannelId) -> Option<ActorId> {
        let groups = self.groups.lock().unwrap();
        let group = groups.get(channel_id)?;
        let own = group.own_leaf_index();
        group
            .members()
            .find(|m| m.index == own)
            .and_then(|m| <[u8; 32]>::try_from(m.credential.serialized_content()).ok())
            .map(ActorId)
    }

    /// The `ActorId`s of every current member of a channel's group (empty if
    /// the channel is unknown to this engine). The MLS leaf credential carries
    /// the member's `ActorId` (see `build_credential_and_signer`), so this is
    /// the authoritative roster — used by welcome-ingest to materialize the
    /// receiver's thread participants.
    pub fn group_members(&self, channel_id: &ChannelId) -> Vec<ActorId> {
        let groups = self.groups.lock().unwrap();
        let Some(group) = groups.get(channel_id) else {
            return Vec::new();
        };
        group
            .members()
            .filter_map(|m| {
                <[u8; 32]>::try_from(m.credential.serialized_content())
                    .ok()
                    .map(ActorId)
            })
            .collect()
    }

    /// Return the current epoch number for the given channel.
    pub fn current_epoch(&self, channel_id: &ChannelId) -> Result<u64> {
        let groups = self.groups.lock().unwrap();
        let group = groups
            .get(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        Ok(group.epoch().as_u64())
    }

    /// Export the current epoch's blob encryption key for a channel.
    ///
    /// Derives a 32-byte key from the MLS group's epoch secret using the
    /// label `"fauna.blob.v1"`. This key is used for encrypting/decrypting
    /// blob data associated with the channel at the current epoch.
    pub fn export_blob_key(&self, channel_id: &ChannelId) -> Result<[u8; 32]> {
        let groups = self.groups.lock().unwrap();
        let group = groups
            .get(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        let secret = group
            .export_secret(self.provider.crypto(), "fauna.blob.v1", &[], 32)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        let mut key = [0u8; 32];
        key.copy_from_slice(&secret);
        Ok(key)
    }

    /// Export the current epoch's **file-chunk** encryption root for a group —
    /// the `chunk_crypto` root for a cross-user-*shared* folder bound to this
    /// group (`mls-group-key-material.md` § Audience: an MLS group at a specific
    /// epoch). Derives a 32-byte secret from the group's epoch secret under label
    /// `"fauna.chunk.v1"` (RFC 9420 § 8.5 exporter; the live realization of that
    /// doc's `:35` "the live integration site will use the exporter"). Domain
    /// separation from `"fauna.blob.v1"` means a group's chunk root and blob key
    /// are independent at the same epoch.
    ///
    /// The returned 32 bytes are fed to `fauna_core::chunk_crypto` as its opaque
    /// `root_secret`; the per-chunk key+nonce then derive from it **and** the
    /// chunk's content hash (convergent dedup, Slice 0). Every member at the same
    /// epoch derives the identical root, so a chunk one member seals, another
    /// opens — the binding's confidentiality boundary.
    ///
    /// Like [`Self::export_blob_key`] this is the **current** epoch's secret and
    /// rotates each epoch; reading chunks sealed at an earlier epoch (history-on-
    /// join across a membership change) is the Slice-3 stable-content-key
    /// refinement (`2026-06-25-shared-file-sets-design.md` § slices/3), not yet
    /// wired.
    pub fn export_chunk_key(&self, channel_id: &ChannelId) -> Result<[u8; 32]> {
        let groups = self.groups.lock().unwrap();
        let group = groups
            .get(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        let secret = group
            .export_secret(self.provider.crypto(), "fauna.chunk.v1", &[], 32)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        let mut key = [0u8; 32];
        key.copy_from_slice(&secret);
        Ok(key)
    }

    /// The blob encryption key for a specific `epoch` — the cached historical
    /// key (native grace-decrypt, populated by [`Self::process_commit`] when the
    /// group advances past an epoch) when present, otherwise the current epoch's
    /// exported key. On wasm (no key cache) this is always the current key, so
    /// cross-epoch grace-decrypt is unavailable there (the same limitation
    /// `fauna-wasm::mls_decrypt_blob` documents). Both the cache and
    /// [`Self::export_blob_key`] produce the MLS-exporter "fauna.blob.v1" secret,
    /// so a key from either path opens a blob sealed by [`Self::seal_conversation_blob`]
    /// at the matching epoch.
    pub fn blob_key_for_epoch(&self, channel_id: &ChannelId, epoch: u64) -> Result<[u8; 32]> {
        #[cfg(feature = "native")]
        if let Ok(Some(key)) = self.storage.get_blob_epoch_key(&channel_id.0, epoch) {
            return Ok(key);
        }
        let _ = epoch;
        self.export_blob_key(channel_id)
    }

    /// Seal `plaintext` as a conversation-attachment blob for `channel_id`:
    /// derive the current epoch's blob key, AEAD-seal under it
    /// ([`crate::blob_crypto::encrypt_blob`]), and return the sealed bytes, their
    /// content-address, and the sealing epoch ([`SealedConvBlob`]). The raw epoch
    /// secret never leaves the engine — the seal happens here, reached by channel
    /// id (tracked internally — constraint: no MLS
    /// epoch secret crosses the FFI). The caller uploads `sealed` to the nest
    /// blob store under `sealed_cid` and stamps `epoch` into the channel
    /// message's [`crate::types::ChannelAttachment`].
    pub fn seal_conversation_blob(
        &self,
        channel_id: &ChannelId,
        plaintext: &[u8],
    ) -> Result<crate::types::SealedConvBlob> {
        let epoch = self.current_epoch(channel_id)?;
        let key = self.export_blob_key(channel_id)?;
        let sealed =
            crate::blob_crypto::encrypt_blob(&key, plaintext).map_err(MlsError::Encoding)?;
        let sealed_cid = *blake3::hash(&sealed).as_bytes();
        Ok(crate::types::SealedConvBlob {
            sealed,
            sealed_cid,
            epoch,
        })
    }

    /// Open a conversation-attachment blob sealed by
    /// [`Self::seal_conversation_blob`]: pick the blob key for the message's
    /// `epoch` (grace-decrypt aware, [`Self::blob_key_for_epoch`]) and AEAD-open
    /// it. The receive-path inverse of [`Self::seal_conversation_blob`].
    pub fn open_conversation_blob(
        &self,
        channel_id: &ChannelId,
        epoch: u64,
        sealed: &[u8],
    ) -> Result<Vec<u8>> {
        let key = self.blob_key_for_epoch(channel_id, epoch)?;
        crate::blob_crypto::decrypt_blob(&key, sealed).map_err(MlsError::Encoding)
    }

    /// Export the current epoch's **folder content-key envelope** key for a
    /// group — the symmetric key under which a shared set's content-key envelope
    /// is sealed (the M2 mechanism, `docs/goal/architecture/key-material-hierarchy.md`
    /// § M2 content-key mechanism). Derives a 32-byte secret from the group's
    /// epoch secret under label `"fauna.fileset-keys.v1"`, domain-separated from
    /// `"fauna.blob.v1"` (conversation blobs) and `"fauna.chunk.v1"` (the
    /// per-epoch chunk root M2 supersedes): the envelope key wraps the *content
    /// keys*, which are themselves the `chunk_crypto` roots, so it must be
    /// independent of them.
    ///
    /// The envelope key is a pure *distribution* key — the MLS group is the
    /// key-distribution channel, the content key inside the envelope is the
    /// durable `chunk_crypto` root. Every member at the same epoch derives the
    /// identical envelope key, so the owner's seal opens for any current member;
    /// a removed member, once the Remove commit advances the epoch, can no longer
    /// derive it (MLS forward secrecy — the basis of rotate-on-removal).
    pub fn export_folder_keys_key(&self, channel_id: &ChannelId) -> Result<[u8; 32]> {
        let groups = self.groups.lock().unwrap();
        let group = groups
            .get(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        let secret = group
            .export_secret(self.provider.crypto(), "fauna.fileset-keys.v1", &[], 32)
            .map_err(|e| MlsError::OpenMls(format!("{e:?}")))?;
        let mut key = [0u8; 32];
        key.copy_from_slice(&secret);
        Ok(key)
    }

    /// Seal a shared folder's **content-key envelope** for `channel_id`: the
    /// owner-side M2 distribution step (`key-material-hierarchy.md:131`). Encodes
    /// the **full** generation bundle (`FolderContentKeys::generations` — every
    /// generation, so a new joiner reads the whole back-catalogue: history-on-join
    /// = FS-NUANCE option (a)) canonically, AEAD-seals it under the current
    /// epoch's envelope key ([`Self::export_folder_keys_key`]), and returns the
    /// sealed bytes plus the sealing epoch ([`SealedContentKeyEnvelope`]).
    ///
    /// The caller publishes `sealed` nest-side opaque
    /// (`fauna.folders.content_key.put`, keyed by group / `ChannelId`),
    /// **re-published on every membership change** (a removal first rotates the
    /// content key and advances the epoch, so the re-seal both distributes the new
    /// generation and excludes the removed member). The raw epoch secret never
    /// leaves the engine — the seal happens here, reached by channel id, exactly
    /// like [`Self::seal_conversation_blob`].
    ///
    /// `set_nonce` is the set's live nonce from the owner's custody, sealed
    /// beside the generations ([`fauna_core::folder_keys::ContentKeyEnvelopePayload`]
    /// — the channel that carries every member the binding its writer-signed
    /// change records verify under).
    pub fn seal_content_key_envelope(
        &self,
        channel_id: &ChannelId,
        keys: &fauna_core::folder_keys::FolderContentKeys,
        set_nonce: Option<[u8; 32]>,
    ) -> Result<crate::types::SealedContentKeyEnvelope> {
        self.seal_content_key_envelope_payload(
            channel_id,
            &fauna_core::folder_keys::ContentKeyEnvelopePayload {
                keys: keys.clone(),
                set_nonce,
                minted_by: None,
                retired_set_nonces: Vec::new(),
                served_at: None,
                unserved_at: None,
            },
        )
    }

    /// [`Self::seal_content_key_envelope`] over the whole payload — the keys,
    /// the live nonce with its minter and the set's lineage
    /// (`writer-signed-change-records.md` ruling (11)(b)). The owner's
    /// signature over the sealed bytes is the caller's
    /// (`fauna_protocol::folder_envelope_sig`): it signs with the identity
    /// key, which this engine never holds.
    pub fn seal_content_key_envelope_payload(
        &self,
        channel_id: &ChannelId,
        payload: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
    ) -> Result<crate::types::SealedContentKeyEnvelope> {
        let epoch = self.current_epoch(channel_id)?;
        let key = self.export_folder_keys_key(channel_id)?;
        let plaintext = payload
            .encode()
            .map_err(|e| MlsError::Encoding(e.to_string()))?;
        let sealed =
            crate::blob_crypto::encrypt_blob(&key, &plaintext).map_err(MlsError::Encoding)?;
        Ok(crate::types::SealedContentKeyEnvelope { sealed, epoch })
    }

    /// Open a content-key envelope sealed by [`Self::seal_content_key_envelope`]:
    /// the member-side M2 read step. Derives the **current** epoch's envelope key
    /// ([`Self::export_folder_keys_key`]), AEAD-opens `sealed`, decodes the
    /// generation bundle, and reconstructs the full [`FolderContentKeys`] history
    /// via `from_generations` (history-on-join).
    ///
    /// Reads at the *current* epoch by design: the envelope is re-published on
    /// every membership change (the only epoch-advancing events for a folder
    /// group in Slices 2–3 are Add and Remove, both of which re-publish), so the
    /// nest always holds the envelope sealed at the reader's current epoch. A
    /// member who has not yet processed the latest membership-change commit (and
    /// so is at an older epoch) fails to decrypt and must catch up first — the
    /// correct fail-closed outcome, not a silent wrong-key read. A removed member
    /// can never derive the post-removal epoch's key, so they cannot open the
    /// re-sealed envelope at all.
    pub fn open_content_key_envelope(
        &self,
        channel_id: &ChannelId,
        sealed: &[u8],
    ) -> Result<fauna_core::folder_keys::FolderContentKeys> {
        self.open_content_key_envelope_payload(channel_id, sealed)
            .map(|payload| payload.keys)
    }

    /// [`Self::open_content_key_envelope`] returning the whole sealed payload —
    /// the generations and the set's nonce, which the member custody-ingest
    /// writes into its own entry.
    pub fn open_content_key_envelope_payload(
        &self,
        channel_id: &ChannelId,
        sealed: &[u8],
    ) -> Result<fauna_core::folder_keys::ContentKeyEnvelopePayload> {
        let key = self.export_folder_keys_key(channel_id)?;
        let plaintext =
            crate::blob_crypto::decrypt_blob(&key, sealed).map_err(MlsError::Encoding)?;
        fauna_core::folder_keys::ContentKeyEnvelopePayload::decode(&plaintext)
            .map_err(|e| MlsError::Encoding(e.to_string()))
    }

    /// Export the current epoch's subscription secret for a channel.
    ///
    /// Derives a 32-byte key from the MLS group's epoch secret using the
    /// label `"fauna.subscription.v1"`. Also returns the current epoch
    /// number so callers can associate the secret with the right epoch.
    /// Per-post encryption keys for subscription tiers are derived from
    /// this secret.
    pub fn export_subscription_secret(&self, channel_id: &ChannelId) -> Result<(u64, [u8; 32])> {
        let groups = self.groups.lock().unwrap();
        let group = groups
            .get(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        let secret = group
            .export_secret(self.provider.crypto(), "fauna.subscription.v1", &[], 32)
            .map_err(|e| MlsError::OpenMls(format!("export_secret: {e:?}")))?;
        let epoch = group.epoch().as_u64();
        let mut key = [0u8; 32];
        key.copy_from_slice(&secret);
        Ok((epoch, key))
    }

    /// Export the current epoch's **room-post** secret for a channel, with the
    /// epoch it belongs to — the base key of a room-restricted post addressed to
    /// an **end-to-end** room (`ui/feed.md` § Encryption at rest →
    /// *Room-restricted — the ruling*, ruling 4: "the MLS epoch secret" at the
    /// arm's `epoch`). The per-post key is `derive_post_key(this, seal_id)`.
    ///
    /// Its own exporter label ([`ROOM_POST_EXPORT_LABEL`]), so at one epoch a
    /// room's posts share a key with none of the group's blobs, chunks or
    /// subscription secret — the separation the ruling asks of the community
    /// class's post kind, given here by the RFC 9420 § 8.5 exporter.
    pub fn export_room_post_secret(&self, channel_id: &ChannelId) -> Result<(u64, [u8; 32])> {
        let groups = self.groups.lock().unwrap();
        let group = groups
            .get(channel_id)
            .ok_or_else(|| MlsError::ChannelNotFound(channel_id.to_string()))?;
        Ok((group.epoch().as_u64(), self.room_post_secret_of(group)?))
    }

    /// The room-post secret of `channel_id` at `epoch` — the group's current
    /// one when it is still at `epoch`, else the one this device kept when its
    /// group advanced past it ([`Self::remember_room_post_secret`]).
    ///
    /// This is how "history falls out of the scheme" for an end-to-end room
    /// (ruling 6): an exporter answers only for the current epoch, so without
    /// the kept secrets every membership change would lock every earlier post
    /// for every member. What it does not do is reach an epoch this device never
    /// saw — a member who joined after it, as MLS intends.
    ///
    /// # Errors
    /// [`MlsError::EpochSecretNotHeld`] when the group is elsewhere and nothing
    /// was kept for `epoch` (always the case on wasm, which keeps no per-epoch
    /// secrets — the limit `blob_key_for_epoch` already carries there).
    pub fn room_post_secret_at(&self, channel_id: &ChannelId, epoch: u64) -> Result<[u8; 32]> {
        {
            let groups = self.groups.lock().unwrap();
            if let Some(group) = groups.get(channel_id)
                && group.epoch().as_u64() == epoch
            {
                return self.room_post_secret_of(group);
            }
        }
        #[cfg(feature = "native")]
        if let Ok(Some(key)) = self.storage.get_room_post_epoch_key(&channel_id.0, epoch) {
            return Ok(key);
        }
        Err(MlsError::EpochSecretNotHeld { epoch })
    }

    /// `group`'s room-post secret at its current epoch.
    fn room_post_secret_of(&self, group: &MlsGroup) -> Result<[u8; 32]> {
        let secret = group
            .export_secret(self.provider.crypto(), ROOM_POST_EXPORT_LABEL, &[], 32)
            .map_err(|e| MlsError::OpenMls(format!("export_secret: {e:?}")))?;
        let mut key = [0u8; 32];
        key.copy_from_slice(&secret);
        Ok(key)
    }

    /// Keep `group`'s room-post secret for its **current** epoch — called just
    /// before every merge that advances the group, whoever authored the
    /// commit, so [`Self::room_post_secret_at`] can still answer for the epoch
    /// being left. Best-effort: a failed keep costs that epoch's posts on this
    /// device and nothing else, so it must never fail the merge it precedes.
    fn remember_room_post_secret(&self, channel_id: &ChannelId, group: &MlsGroup) {
        #[cfg(feature = "native")]
        if let Ok(key) = self.room_post_secret_of(group) {
            let _ =
                self.storage
                    .put_room_post_epoch_key(&channel_id.0, group.epoch().as_u64(), &key);
        }
        #[cfg(not(feature = "native"))]
        let _ = (channel_id, group);
    }

    /// Find the DM peer in a 2-member group (the member whose credential is not ours).
    pub fn extract_dm_peer(&self, channel_id: &ChannelId) -> Option<ActorId> {
        let my_id = self.identity_actor_id();
        let groups = self.groups.lock().unwrap();
        let group = groups.get(channel_id)?;
        for member in group.members() {
            let cred_bytes = member.credential.serialized_content();
            if cred_bytes.len() == 32 && cred_bytes != my_id.0.as_slice() {
                let mut peer = [0u8; 32];
                peer.copy_from_slice(cred_bytes);
                return Some(ActorId(peer));
            }
        }
        None
    }

    #[cfg(feature = "native")]
    pub fn put_dm_channel(&self, peer: &[u8; 32], channel: &[u8; 32]) -> Result<()> {
        self.storage
            .put_dm_channel(peer, channel)
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    #[cfg(feature = "native")]
    pub fn get_dm_channel(&self, peer: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        self.storage
            .get_dm_channel(peer)
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    #[cfg(feature = "native")]
    pub fn list_dm_channels(&self) -> Result<Vec<([u8; 32], [u8; 32])>> {
        self.storage
            .list_dm_channels()
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    #[cfg(feature = "native")]
    pub fn put_channel_nest_url(&self, channel_id: &ChannelId, nest_url: &str) -> Result<()> {
        self.storage
            .put_channel_nest_url(&channel_id.0, nest_url)
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    #[cfg(feature = "native")]
    pub fn get_channel_nest_url(&self, channel_id: &ChannelId) -> Result<Option<String>> {
        self.storage
            .get_channel_nest_url(&channel_id.0)
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    #[cfg(feature = "native")]
    pub fn set_read_timestamp(&self, peer: &[u8; 32], timestamp_ms: u64) -> Result<()> {
        self.storage
            .set_read_timestamp(peer, timestamp_ms)
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    #[cfg(feature = "native")]
    pub fn get_read_timestamp(&self, peer: &[u8; 32]) -> Result<u64> {
        self.storage
            .get_read_timestamp(peer)
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    #[cfg(feature = "native")]
    pub fn put_group_channel(&self, group_id: &str, channel: &[u8; 32]) -> Result<()> {
        self.storage
            .put_group_channel(group_id, channel)
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    #[cfg(feature = "native")]
    pub fn get_group_channel(&self, group_id: &str) -> Result<Option<[u8; 32]>> {
        self.storage
            .get_group_channel(group_id)
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    #[cfg(feature = "native")]
    pub fn list_group_channels(&self) -> Result<Vec<(String, [u8; 32])>> {
        self.storage
            .list_group_channels()
            .map_err(|e| MlsError::Storage(e.to_string()))
    }

    pub fn validate_key_package(&self, bytes: &[u8]) -> Result<KeyPackage> {
        let kp_in = KeyPackageIn::tls_deserialize_exact(bytes)
            .map_err(|e| MlsError::Encoding(format!("TLS deserialize key package: {e:?}")))?;
        kp_in
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .map_err(|e| MlsError::OpenMls(format!("validate key package: {e:?}")))
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::types::{ChannelEnvelope, ChannelMessageBody};
    use fauna_core::data::Timestamp;
    use openmls::prelude::tls_codec::Serialize as TlsSerializeTrait;
    use tempfile::NamedTempFile;

    /// Slice 4c: `process_commit` classifies its failures so the inbound driver
    /// dispatches without string-matching (`devices.md` § Cross-device MLS
    /// group-state sync; design §3 "own-leaf foreign commit = resync signal").
    /// A twin device (same identity, replica-restored, same leaf) processing
    /// the other device's current-epoch commit gets the typed resync signal
    /// carrying the commit's epoch; an already-merged own commit and a replayed
    /// foreign commit both classify as past-epoch (quiet skip).
    /// **Why key-package replenish must run AFTER a replica restore** (the
    /// login ordering every leg follows — `devices.md` § Cross-device MLS
    /// group-state sync): a provider-storage restore **swaps** the whole KV, so
    /// a key package minted before it loses its private init key — a peer who
    /// fetched that package from the nest pool then mints a group whose Welcome
    /// this device can never join (the web slice-6 bug class). Minted after the
    /// restore, the join works. This pins the swap semantics the ordering rule
    /// exists for.
    #[test]
    fn key_package_minted_before_provider_swap_loses_its_init_key() {
        use crate::state_replica::ProviderReplica;
        let alice = MlsEngine::new_in_memory(ActorKeypair::from_secret([1u8; 32])).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([2u8; 32])).unwrap();
        // The state the replica will restore (any established snapshot).
        let replica = ProviderReplica::from_engine(&alice);

        // Hazard order: mint (+ publish to the nest pool), THEN restore.
        let kps = alice.generate_key_packages(1).unwrap();
        replica.restore_into_unchecked(&alice).unwrap();
        let (_, welcome) = bob.create_group(&kps).unwrap();
        assert!(
            alice.join_from_welcome(welcome).is_err(),
            "the provider swap wiped the minted package's private init key"
        );

        // Correct order: restore, THEN mint.
        let kps = alice.generate_key_packages(1).unwrap();
        let (_, welcome) = bob.create_group(&kps).unwrap();
        alice
            .join_from_welcome(welcome)
            .expect("a package minted after the restore joins its Welcome");
    }

    #[test]
    fn process_commit_classifies_own_leaf_and_past_epoch() {
        use crate::state_replica::ProviderReplica;
        // Alice (device 1) + Bob two-member group.
        let alice1 = MlsEngine::new_in_memory(ActorKeypair::from_secret([1u8; 32])).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([2u8; 32])).unwrap();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, welcome) = alice1.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();

        // Alice device 2 = replica restore (same identity, same leaf).
        let alice2 = MlsEngine::new_in_memory(ActorKeypair::from_secret([1u8; 32])).unwrap();
        ProviderReplica::from_engine(&alice1)
            .restore_into_unchecked(&alice2)
            .unwrap();
        let restore_epoch = alice2.current_epoch(&channel).unwrap();
        assert_eq!(
            restore_epoch,
            alice1.current_epoch(&channel).unwrap(),
            "twin device restored at the same epoch"
        );

        // Device 1 commits (takeover) and merges.
        let own_commit = alice1.self_update(&channel).unwrap();
        alice1.merge_pending_commit(&channel).unwrap();

        // Own already-merged commit coming back around the poll → past-epoch.
        let err = alice1
            .process_commit(&channel, &own_commit)
            .expect_err("own merged commit must not process");
        assert!(matches!(err, MlsError::PastEpochCommit), "got {err:?}");

        // The twin (still at the pre-commit epoch) meets the own-leaf commit →
        // the typed resync signal, carrying the commit's (= the twin's current)
        // epoch for the crash-window pending-merge equality check.
        let err = alice2
            .process_commit(&channel, &own_commit)
            .expect_err("own-leaf foreign commit cannot be processed by MLS");
        assert!(
            matches!(err, MlsError::OwnLeafCommit { epoch } if epoch == restore_epoch),
            "got {err:?}"
        );

        // A replayed foreign commit (Bob processes it twice) → past-epoch.
        bob.process_commit(&channel, &own_commit).unwrap();
        let err = bob
            .process_commit(&channel, &own_commit)
            .expect_err("replayed foreign commit must not process");
        assert!(matches!(err, MlsError::PastEpochCommit), "got {err:?}");
    }

    // ── Folder commit policy (owner-managed roster) ─────────────────────────
    //
    // The member-side enforcement of `federation.md` § Cross-nest shared
    // folders + channel append: the nest admits any rostered member's Commit
    // (it cannot read PrivateMessage commit content), and every honest member
    // refuses a proposal-carrying commit whose authenticated committer is not
    // the channel's recorded folder owner — while a bare self-`Update` (the
    // device-owned-epoch takeover) merges from any member.

    /// A three-seat owner/member/member fixture: alice owns, bob and carol are
    /// members joined from alice's Welcome. Returns the engines + channel +
    /// alice's ActorId.
    fn owner_two_members_fixture() -> (MlsEngine, MlsEngine, MlsEngine, ChannelId, ActorId) {
        let alice = MlsEngine::new_in_memory(ActorKeypair::from_secret([1u8; 32])).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([2u8; 32])).unwrap();
        let carol = MlsEngine::new_in_memory(ActorKeypair::from_secret([3u8; 32])).unwrap();
        let owner_actor = ActorKeypair::from_secret([1u8; 32]).actor_id();
        let mut kps = bob.generate_key_packages(1).unwrap();
        kps.extend(carol.generate_key_packages(1).unwrap());
        let (channel, welcome) = alice.create_group(&kps).unwrap();
        bob.join_from_welcome(welcome.clone()).unwrap();
        carol.join_from_welcome(welcome).unwrap();
        (alice, bob, carol, channel, owner_actor)
    }

    /// The join records the Welcome's MLS-authenticated sender — the identity
    /// the folder join stamps as the channel's owner.
    #[test]
    fn a_join_records_the_welcome_sender_identity() {
        let (_alice, bob, carol, channel, owner_actor) = owner_two_members_fixture();
        assert_eq!(bob.welcome_sender(&channel), Some(owner_actor));
        assert_eq!(carol.welcome_sender(&channel), Some(owner_actor));
    }

    /// The join also records one history admission, and only the inviter's
    /// account, in the epoch of the join, spends it — once. A stranger or a
    /// later epoch spends nothing, and the group's creator never held one.
    #[test]
    fn a_join_records_one_history_admission_only_its_inviter_spends() {
        let (alice, bob, _carol, channel, owner_actor) = owner_two_members_fixture();
        let joined_at = bob.current_epoch(&channel).unwrap();
        let stranger = ActorKeypair::from_secret([9u8; 32]).actor_id();
        assert!(!bob.take_history_admission(&channel, &stranger, joined_at));
        assert!(!bob.take_history_admission(&channel, &owner_actor, joined_at + 1));
        assert!(bob.take_history_admission(&channel, &owner_actor, joined_at));
        assert!(!bob.take_history_admission(&channel, &owner_actor, joined_at));
        assert!(!alice.take_history_admission(&channel, &owner_actor, joined_at));
    }

    /// A member's bare self-`Update` takeover merges on an owner-managed
    /// folder channel — the device-owned-epoch invariant's legitimate member
    /// commit, the exact shape the share-plane advertisement door posts.
    #[test]
    fn a_member_self_update_takeover_merges_on_an_owner_managed_channel() {
        let (alice, bob, carol, channel, owner_actor) = owner_two_members_fixture();
        for e in [&alice, &bob, &carol] {
            e.mark_folder_channel_owner(&channel, &owner_actor);
        }
        let takeover = bob.self_update(&channel).unwrap();
        bob.merge_pending_commit(&channel).unwrap();
        alice
            .process_commit(&channel, &takeover)
            .expect("the owner merges a member's bare self-update takeover");
        carol
            .process_commit(&channel, &takeover)
            .expect("a co-member merges a member's bare self-update takeover");
    }

    /// A proposal-carrying commit from a NON-owner member is refused with the
    /// typed policy verdict — and a replay of the same bytes repeats that
    /// verdict from the durable pre-decrypt memo instead of misclassifying as
    /// a `SecretReuseError` stall (the every-launch folder-rail re-walk).
    #[test]
    fn a_non_owner_proposal_commit_is_refused_and_its_replay_skips_pre_decrypt() {
        let (alice, bob, _carol, channel, owner_actor) = owner_two_members_fixture();
        alice.mark_folder_channel_owner(&channel, &owner_actor);

        // Bob (a member, not the owner) commits a Remove of carol.
        let carol_actor = ActorKeypair::from_secret([3u8; 32]).actor_id();
        let carol_leaf = bob.find_leaf_by_identity(&channel, &carol_actor).unwrap();
        let hostile = bob.remove_member_staged(&channel, carol_leaf).unwrap();

        let err = alice
            .process_commit(&channel, &hostile)
            .expect_err("a non-owner Remove commit must not merge");
        assert!(
            matches!(err, MlsError::PolicyRefusedCommit { .. }),
            "got {err:?}"
        );

        // The replay (same bytes, e.g. next launch's re-walk from seq 0) hits
        // the durable memo BEFORE decryption and repeats the typed verdict —
        // without the memo this replay is a SecretReuseError local stall.
        let err = alice
            .process_commit(&channel, &hostile)
            .expect_err("the replay must repeat the refusal");
        assert!(
            matches!(err, MlsError::PolicyRefusedCommit { .. }),
            "got {err:?}"
        );

        // The group did not advance: the owner's next legitimate commit (built
        // at the unadvanced epoch) still merges everywhere.
        let rotate = alice.self_update(&channel).unwrap();
        alice.merge_pending_commit(&channel).unwrap();
        bob.clear_pending_commit(&channel).unwrap();
        bob.process_commit(&channel, &rotate)
            .expect("the owner's next commit merges past the refused record");
    }

    /// The owner's own proposal-carrying commit (the rotate-on-removal shape)
    /// merges on an owner-managed channel.
    #[test]
    fn the_owners_proposal_commit_merges_on_an_owner_managed_channel() {
        let (alice, bob, _carol, channel, owner_actor) = owner_two_members_fixture();
        bob.mark_folder_channel_owner(&channel, &owner_actor);
        let carol_actor = ActorKeypair::from_secret([3u8; 32]).actor_id();
        let carol_leaf = alice.find_leaf_by_identity(&channel, &carol_actor).unwrap();
        let remove = alice.remove_member_staged(&channel, carol_leaf).unwrap();
        alice.merge_pending_commit(&channel).unwrap();
        bob.process_commit(&channel, &remove)
            .expect("the owner's Remove merges for a policy-holding member");
    }

    /// An unstamped channel (chat, scheduling, not-yet-bound) keeps today's open
    /// commit processing — the policy fires only on the positive owner marker.
    #[test]
    fn an_unstamped_channel_keeps_open_commit_processing() {
        let (alice, bob, _carol, channel, _owner_actor) = owner_two_members_fixture();
        let carol_actor = ActorKeypair::from_secret([3u8; 32]).actor_id();
        let carol_leaf = bob.find_leaf_by_identity(&channel, &carol_actor).unwrap();
        let remove = bob.remove_member_staged(&channel, carol_leaf).unwrap();
        alice
            .process_commit(&channel, &remove)
            .expect("no owner marker → any member's proposal commit merges (unchanged)");
    }

    /// The fixture above is minted via the inherent [`MlsEngine::create_group`],
    /// which never stamps; the folder adapter's mint
    /// (`fauna_client_folders::mls_adapter`) stamps right after it. This pins
    /// that the stamp ALONE arms the policy: once it lands, the SAME channel
    /// refuses a non-owner proposal commit exactly like a channel stamped at
    /// birth.
    #[test]
    fn stamping_the_owner_marker_arms_the_policy_on_an_unstamped_channel() {
        let (alice, bob, _carol, channel, owner_actor) = owner_two_members_fixture();
        assert_eq!(
            alice.folder_channel_owner(&channel),
            None,
            "an inherent-minted seat holds no marker until stamped"
        );

        // The adapter's exact call.
        alice.mark_folder_channel_owner(&channel, &owner_actor);
        assert_eq!(alice.folder_channel_owner(&channel), Some(owner_actor));

        let carol_actor = ActorKeypair::from_secret([3u8; 32]).actor_id();
        let carol_leaf = bob.find_leaf_by_identity(&channel, &carol_actor).unwrap();
        let hostile = bob.remove_member_staged(&channel, carol_leaf).unwrap();
        let err = alice
            .process_commit(&channel, &hostile)
            .expect_err("once stamped, a non-owner Remove commit must not merge");
        assert!(
            matches!(err, MlsError::PolicyRefusedCommit { .. }),
            "got {err:?}"
        );
    }

    /// The successor's own-seat re-stamp: a marker
    /// naming an attested predecessor moves to this identity; a marker naming
    /// anyone else — a channel the predecessor merely joined — stays, and a
    /// predecessor the caller did not attest moves nothing.
    #[test]
    fn restamping_moves_only_markers_that_name_an_attested_predecessor() {
        let successor = MlsEngine::new_in_memory(ActorKeypair::from_secret([4u8; 32])).unwrap();
        let successor_actor = ActorKeypair::from_secret([4u8; 32]).actor_id();
        let retired = ActorKeypair::from_secret([1u8; 32]).actor_id();
        let other_owner = ActorKeypair::from_secret([2u8; 32]).actor_id();
        let unattested = ActorKeypair::from_secret([3u8; 32]).actor_id();
        let owned = ChannelId([0x11; 32]);
        let joined = ChannelId([0x22; 32]);
        let someone_elses = ChannelId([0x33; 32]);
        successor.mark_folder_channel_owner(&owned, &retired);
        successor.mark_folder_channel_owner(&joined, &other_owner);
        successor.mark_folder_channel_owner(&someone_elses, &unattested);

        assert_eq!(successor.restamp_folder_owner_markers(&[retired]), 1);
        assert_eq!(
            successor.folder_channel_owner(&owned),
            Some(successor_actor)
        );
        assert_eq!(successor.folder_channel_owner(&joined), Some(other_owner));
        assert_eq!(
            successor.folder_channel_owner(&someone_elses),
            Some(unattested)
        );

        assert_eq!(
            successor.restamp_folder_owner_markers(&[retired]),
            0,
            "idempotent: nothing left naming the retired key"
        );
        assert_eq!(
            successor.restamp_folder_owner_markers(&[]),
            0,
            "no attested predecessor moves nothing"
        );
    }

    /// The at-rest folder park: one slot per channel, latest wins, listed by
    /// channel, forgotten on demand — and dropped with the group.
    #[test]
    fn the_folder_park_rests_per_channel_and_goes_with_the_group() {
        let alice = MlsEngine::new_in_memory(ActorKeypair::from_secret([1u8; 32])).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([2u8; 32])).unwrap();
        let (channel, _welcome) = alice
            .create_group(&bob.generate_key_packages(1).unwrap())
            .unwrap();
        let other = ChannelId([0x44; 32]);

        alice.park_folder_succession(&channel, b"first");
        alice.park_folder_succession(&channel, b"latest");
        alice.park_folder_succession(&other, b"elsewhere");
        assert_eq!(
            alice.parked_folder_succession(&channel).as_deref(),
            Some(&b"latest"[..]),
            "one slot per channel, latest wins"
        );
        let mut listed = alice.parked_folder_successions();
        listed.sort_by_key(|(c, _)| c.0);
        let mut expected = vec![
            (channel, b"latest".to_vec()),
            (other, b"elsewhere".to_vec()),
        ];
        expected.sort_by_key(|(c, _)| c.0);
        assert_eq!(listed, expected);

        assert!(alice.forget_parked_folder_succession(&other));
        assert!(!alice.forget_parked_folder_succession(&other), "idempotent");

        alice.forget_group(&channel).unwrap();
        assert_eq!(
            alice.parked_folder_succession(&channel),
            None,
            "the park goes with the group"
        );
    }

    /// The stale member's **only** healer, pinned end-to-end.
    ///
    /// A member's engine can come back at a stale epoch: the local provider
    /// snapshot is written only incidentally ([`MlsEngine::save_state`] has no
    /// caller outside `persist_group`, i.e. group create/join), and the nest
    /// `provider` replica is saved on a debounced observer tick. Nothing ties
    /// either snapshot to an epoch advance. What rescues such a member on the
    /// **folder rail** is that its commit poll re-walks the channel log from
    /// seq 0 on every launch (`fauna_conversations::session`'s `fs_cursors`, a
    /// per-process map seeded to 0), and re-applying an already-folded commit is
    /// a quiet [`MlsError::PastEpochCommit`] skip rather than a hard error — so
    /// the walk runs past the commits the stale snapshot already contains and
    /// applies exactly the ones it is missing.
    ///
    /// This test is that argument, executed against real openmls: if the re-walk
    /// ever stops being idempotent, or a past-epoch commit stops being skippable,
    /// a stale folder member silently stops converging. Pinned here because the
    /// mechanism lives in three crates and no single one of them owned the proof.
    #[test]
    fn stale_reloaded_member_rewalks_log_from_zero_and_reaches_current_epoch() {
        use crate::state_replica::ProviderReplica;

        let alice = MlsEngine::new_in_memory(ActorKeypair::from_secret([1u8; 32])).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([2u8; 32])).unwrap();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, welcome) = alice.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();

        // c1 lands and bob folds it — the steady-state poll.
        let c1 = alice.self_update(&channel).unwrap();
        alice.merge_pending_commit(&channel).unwrap();
        bob.process_commit(&channel, &c1).unwrap();

        // Snapshot bob HERE: this is the stale provider a crash comes back on.
        // (The autosave tick that captured it was fired by unrelated activity —
        // nothing about c1 caused this snapshot to exist.)
        let stale = ProviderReplica::from_engine(&bob);

        // Two more commits land; the live bob folds them and sits at the head.
        let c2 = alice.self_update(&channel).unwrap();
        alice.merge_pending_commit(&channel).unwrap();
        bob.process_commit(&channel, &c2).unwrap();
        let c3 = alice.self_update(&channel).unwrap();
        alice.merge_pending_commit(&channel).unwrap();
        bob.process_commit(&channel, &c3).unwrap();

        let head = alice.current_epoch(&channel).unwrap();
        assert_eq!(bob.current_epoch(&channel).unwrap(), head);

        // Crash + reload: a fresh engine under bob's identity, restored from the
        // stale snapshot. It is two epochs behind and has never seen c2/c3.
        let bob_reloaded = MlsEngine::new_in_memory(ActorKeypair::from_secret([2u8; 32])).unwrap();
        stale.restore_into_unchecked(&bob_reloaded).unwrap();
        let restored_epoch = bob_reloaded.current_epoch(&channel).unwrap();
        assert!(
            restored_epoch < head,
            "restored at {restored_epoch}, head is {head} — the fixture must reload a STALE engine"
        );

        // The re-walk, exactly as the folder poll drives it on a fresh launch:
        // every commit from seq 0, in nest order, past-epoch treated as a skip.
        let mut skipped = 0usize;
        for commit in [&c1, &c2, &c3] {
            match bob_reloaded.process_commit(&channel, commit) {
                Ok(()) => {}
                Err(MlsError::PastEpochCommit) => skipped += 1,
                Err(e) => panic!("the re-walk must never hard-error: {e:?}"),
            }
        }

        assert_eq!(
            skipped, 1,
            "only c1 was already folded into the stale snapshot"
        );
        assert_eq!(
            bob_reloaded.current_epoch(&channel).unwrap(),
            head,
            "the stale member self-healed to the head epoch by re-walking from 0"
        );
    }

    /// openMLS reports an epoch mismatch in **either** direction as
    /// `ValidationError::WrongEpoch`; [`MlsError::PastEpochCommit`] and
    /// [`MlsError::FutureEpochCommit`] split them, and the split is what keeps a
    /// skipped commit from disappearing.
    ///
    /// A member whose durable ingest cursor outran its durable provider resumes the
    /// poll *after* a commit it never applied. Every later commit is then for an
    /// epoch **ahead** of its own, and before this split each one was quiet-skipped
    /// as `PastEpochCommit` — the same arm as "my own merged commit came back
    /// around". So the device sat at a dead epoch, decrypting nothing new, forever,
    /// and never logged a thing. The two cases must not share an arm.
    #[test]
    fn commit_for_a_future_epoch_is_not_mistaken_for_an_already_merged_one() {
        use crate::state_replica::ProviderReplica;

        let alice = MlsEngine::new_in_memory(ActorKeypair::from_secret([1u8; 32])).unwrap();
        let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([2u8; 32])).unwrap();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, welcome) = alice.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();

        // c1 lands; bob folds it. Snapshot bob here — this is the durable provider.
        let c1 = alice.self_update(&channel).unwrap();
        alice.merge_pending_commit(&channel).unwrap();
        bob.process_commit(&channel, &c1).unwrap();
        let durable = ProviderReplica::from_engine(&bob);

        // c2 and c3 land while bob is live.
        let c2 = alice.self_update(&channel).unwrap();
        alice.merge_pending_commit(&channel).unwrap();
        let c3 = alice.self_update(&channel).unwrap();
        alice.merge_pending_commit(&channel).unwrap();

        // Bob relaunches on the durable provider (epoch of c1), but with a cursor
        // that wrongly claims c2 was folded — the torn-pair strand. His poll resumes
        // at c3.
        let bob_reloaded = MlsEngine::new_in_memory(ActorKeypair::from_secret([2u8; 32])).unwrap();
        durable.restore_into_unchecked(&bob_reloaded).unwrap();
        let stale_epoch = bob_reloaded.current_epoch(&channel).unwrap();

        let err = bob_reloaded
            .process_commit(&channel, &c3)
            .expect_err("c3 applies to an epoch bob never reached");
        match err {
            MlsError::FutureEpochCommit { epoch } => assert!(
                epoch > stale_epoch,
                "c3's epoch {epoch} must be ahead of bob's {stale_epoch}"
            ),
            other => panic!("a skipped commit must be loud, not a quiet skip: got {other:?}"),
        }

        // The other direction still classifies as before: c1 is already folded.
        assert!(
            matches!(
                bob_reloaded.process_commit(&channel, &c1),
                Err(MlsError::PastEpochCommit)
            ),
            "an already-merged commit stays a quiet skip"
        );

        // And the hole is closed by applying the commit that was skipped, so a
        // cursor rewind + idempotent re-walk is a sound heal.
        bob_reloaded.process_commit(&channel, &c2).unwrap();
        bob_reloaded.process_commit(&channel, &c3).unwrap();
        assert_eq!(
            bob_reloaded.current_epoch(&channel).unwrap(),
            alice.current_epoch(&channel).unwrap(),
            "re-walking the gap heals the strand"
        );
    }

    fn make_engine() -> (MlsEngine, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let identity = ActorKeypair::generate();
        let engine = MlsEngine::new(identity, tmp.path()).unwrap();
        (engine, tmp)
    }

    #[test]
    fn create_engine_and_generate_key_packages() {
        let (engine, _tmp) = make_engine();

        let packages = engine.generate_key_packages(5).unwrap();
        assert_eq!(packages.len(), 5);

        // Verify the identity accessor works.
        let _ = engine.identity_actor_id();
    }

    #[test]
    fn create_group_and_join_via_welcome() {
        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();

        // Bob generates a key package for Alice to use.
        let bob_kps = bob.generate_key_packages(1).unwrap();

        // Alice creates a group with Bob.
        let (alice_channel_id, welcome) = alice.create_group(&bob_kps).unwrap();

        // Bob joins from the Welcome.
        let bob_channel_id = bob.join_from_welcome(welcome).unwrap();

        assert_eq!(alice_channel_id, bob_channel_id);
    }

    // ── export_chunk_key — the shared-folder chunk_crypto root (Slice 1) ──

    #[test]
    fn export_chunk_key_roundtrips_via_chunk_crypto() {
        use fauna_core::data::ContentHash;

        let (alice, _tmp) = make_engine();
        let channel_id = alice.create_solo_group().unwrap();
        let root = alice.export_chunk_key(&channel_id).unwrap();

        // A bound set seals its chunks under the group-derived root and reads
        // them back — the Slice-1 success criterion at the primitive boundary.
        let plain = b"the quick brown fox".to_vec();
        let hash = ContentHash::of_raw(&plain);
        let (_, sealed) = fauna_core::chunk_seal::seal_chunk_body(&hash, &plain, &root).unwrap();
        let opened = fauna_core::chunk_crypto::decrypt_chunk(&root, &hash, &sealed).unwrap();
        assert_eq!(
            fauna_core::compress::unframe_verified_chunk(opened, &hash).unwrap(),
            plain
        );
        // The seal is real — ciphertext differs from plaintext.
        assert_ne!(sealed, plain);
    }

    #[test]
    fn export_chunk_key_is_shared_across_members() {
        // Every member at the same epoch derives the identical chunk root, so a
        // chunk one member seals, another opens (the binding's confidentiality
        // boundary). This is what makes a *cross-user* shared set work.
        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (alice_cid, welcome) = alice.create_group(&bob_kps).unwrap();
        let bob_cid = bob.join_from_welcome(welcome).unwrap();
        assert_eq!(alice_cid, bob_cid);

        assert_eq!(
            alice.export_chunk_key(&alice_cid).unwrap(),
            bob.export_chunk_key(&bob_cid).unwrap(),
            "members at the same epoch must derive the same chunk root"
        );
    }

    #[test]
    fn export_chunk_key_distinct_per_group_and_from_blob_key() {
        let (alice, _tmp) = make_engine();
        let g1 = alice.create_solo_group().unwrap();
        let g2 = alice.create_solo_group().unwrap();
        assert_ne!(g1, g2);
        assert_ne!(
            alice.export_chunk_key(&g1).unwrap(),
            alice.export_chunk_key(&g2).unwrap(),
            "distinct groups must derive distinct chunk roots"
        );
        // Domain separation: the chunk root and blob key of one group at one
        // epoch are independent (different exporter labels).
        assert_ne!(
            alice.export_chunk_key(&g1).unwrap(),
            alice.export_blob_key(&g1).unwrap(),
            "fauna.chunk.v1 and fauna.blob.v1 must be domain-separated"
        );
    }

    #[test]
    fn export_chunk_key_unknown_group_errors() {
        use crate::types::ChannelId;
        let (alice, _tmp) = make_engine();
        let bogus = ChannelId::from_group_id(&[0x42u8; 32]);
        assert!(alice.export_chunk_key(&bogus).is_err());
    }

    // ── content-key envelope — the M2 generation-bundle distribution (Slice 3) ──

    use fauna_core::folder_keys::FolderContentKeys;

    #[test]
    fn content_key_envelope_roundtrips_full_history_for_a_member() {
        // Owner holds a 3-generation history; seals the envelope; a member at the
        // same epoch opens it and reconstructs the *full* back-catalogue
        // (history-on-join, FS-NUANCE option (a)).
        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (alice_cid, welcome) = alice.create_group(&bob_kps).unwrap();
        let bob_cid = bob.join_from_welcome(welcome).unwrap();
        assert_eq!(alice_cid, bob_cid);

        let mut owner_keys = FolderContentKeys::genesis([1u8; 32], 1_000);
        owner_keys.rotate([2u8; 32], 2_000);
        owner_keys.rotate([3u8; 32], 3_000);

        let envelope = alice
            .seal_content_key_envelope(&alice_cid, &owner_keys, None)
            .unwrap();
        assert_eq!(envelope.epoch, alice.current_epoch(&alice_cid).unwrap());
        // The seal is real — sealed bytes are not the plaintext bundle.
        assert!(!envelope.sealed.is_empty());

        let member_keys = bob
            .open_content_key_envelope(&bob_cid, &envelope.sealed)
            .unwrap();
        assert_eq!(
            member_keys, owner_keys,
            "member reconstructs the full history"
        );
        assert_eq!(member_keys.current_version(), 3);
        assert_eq!(member_keys.key_for(1), Some(&[1u8; 32]));
        assert_eq!(member_keys.key_for(2), Some(&[2u8; 32]));
        assert_eq!(member_keys.key_for(3), Some(&[3u8; 32]));
    }

    #[test]
    fn content_key_envelope_owner_opens_its_own_seal() {
        // A solo owner seals + opens (the bind-time genesis case).
        let (alice, _tmp) = make_engine();
        let cid = alice.create_solo_group().unwrap();
        let keys = FolderContentKeys::genesis([9u8; 32], 1_000);
        let envelope = alice.seal_content_key_envelope(&cid, &keys, None).unwrap();
        let opened = alice
            .open_content_key_envelope(&cid, &envelope.sealed)
            .unwrap();
        assert_eq!(opened, keys);
    }

    #[test]
    fn content_key_envelope_key_is_domain_separated() {
        // The envelope key must be independent of the chunk root and blob key it
        // wraps/coexists with at the same epoch (distinct exporter labels).
        let (alice, _tmp) = make_engine();
        let cid = alice.create_solo_group().unwrap();
        let env_key = alice.export_folder_keys_key(&cid).unwrap();
        assert_ne!(env_key, alice.export_chunk_key(&cid).unwrap());
        assert_ne!(env_key, alice.export_blob_key(&cid).unwrap());
    }

    #[test]
    fn content_key_envelope_non_member_cannot_open() {
        // A different group's member (a stand-in for a removed member, whose
        // post-removal epoch key differs) cannot derive the envelope key and so
        // cannot open the seal — fail-closed, the rotate-on-removal basis.
        let (alice, _tmp_a) = make_engine();
        let (carol, _tmp_c) = make_engine();
        let alice_cid = alice.create_solo_group().unwrap();
        let carol_cid = carol.create_solo_group().unwrap();

        let keys = FolderContentKeys::genesis([5u8; 32], 1_000);
        let envelope = alice
            .seal_content_key_envelope(&alice_cid, &keys, None)
            .unwrap();
        // Carol's own group derives a different envelope key → AEAD open fails.
        assert!(
            carol
                .open_content_key_envelope(&carol_cid, &envelope.sealed)
                .is_err()
        );
    }

    #[test]
    fn content_key_envelope_unknown_group_errors() {
        let (alice, _tmp) = make_engine();
        let bogus = ChannelId::from_group_id(&[0x42u8; 32]);
        let keys = FolderContentKeys::genesis([1u8; 32], 1_000);
        assert!(
            alice
                .seal_content_key_envelope(&bogus, &keys, None)
                .is_err()
        );
        assert!(alice.open_content_key_envelope(&bogus, &[0u8; 64]).is_err());
    }

    #[test]
    fn key_package_from_bytes_roundtrips_into_create_group() {
        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();

        // Bob publishes a key package as wire bytes (the `keypackage.upload`
        // shape the nest stores and `keypackage.fetch` returns).
        let bob_kp_bytes = bob.generate_key_packages_bytes(1).unwrap();

        // Alice deserializes+validates it back into a `KeyPackage` (the inbound
        // counterpart `create_group` needs) and bootstraps a group with Bob.
        let bob_kp = alice.key_package_from_bytes(&bob_kp_bytes[0]).unwrap();
        let (channel_id, welcome) = alice.create_group(std::slice::from_ref(&bob_kp)).unwrap();
        let bob_channel_id = bob.join_from_welcome(welcome).unwrap();
        assert_eq!(channel_id, bob_channel_id);
    }

    #[test]
    fn key_package_from_bytes_rejects_garbage() {
        let (alice, _tmp_a) = make_engine();
        assert!(
            alice
                .key_package_from_bytes(&[0xde, 0xad, 0xbe, 0xef])
                .is_err()
        );
    }

    #[test]
    fn last_resort_key_package_is_valid_and_marked_last_resort() {
        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();

        // Mint one last-resort KP (the onboarding publication shape, Spec Y2).
        let lr_bytes = alice.generate_last_resort_key_package_bytes().unwrap();

        // It parses back as a valid key package (the inbound validate path peers
        // run on `keypackage.fetch`).
        let lr_kp = bob.key_package_from_bytes(&lr_bytes).unwrap();

        // The MLS `last_resort` extension is set — this is what tells the nest to
        // keep it reusable (`take_key_package` never consumes it).
        assert!(
            lr_kp.last_resort(),
            "the minted key package carries the MLS last_resort extension"
        );

        // A normal one-time KP must NOT carry the extension (so the nest treats
        // it as a consumable pool entry).
        let ot_bytes = alice.generate_key_packages_bytes(1).unwrap();
        let ot_kp = bob.key_package_from_bytes(&ot_bytes[0]).unwrap();
        assert!(
            !ot_kp.last_resort(),
            "a normal one-time key package is not last-resort"
        );

        // And it still works as a real group-bootstrap key package.
        let (channel_id, welcome) = bob.create_group(std::slice::from_ref(&lr_kp)).unwrap();
        let alice_channel = alice.join_from_welcome(welcome).unwrap();
        assert_eq!(channel_id, alice_channel);
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let bob_channel_id = bob.join_from_welcome(welcome).unwrap();
        assert_eq!(channel_id, bob_channel_id);

        // Alice sends a message.
        let msg = ChannelMessage {
            sender: alice.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("hello from alice".into()),
            timestamp: Timestamp::now(),
        };
        let ciphertext = alice.encrypt(&channel_id, &msg).unwrap();

        // Bob decrypts Alice's message.
        let decrypted = bob.decrypt(&channel_id, &ciphertext).unwrap();
        assert_eq!(decrypted.sender, alice.identity_actor_id());
        if let ChannelMessageBody::Text(text) = &decrypted.body {
            assert_eq!(text, "hello from alice");
        } else {
            panic!("expected Text body");
        }

        // Bob sends a message back.
        let msg2 = ChannelMessage {
            sender: bob.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("hello from bob".into()),
            timestamp: Timestamp::now(),
        };
        let ciphertext2 = bob.encrypt(&channel_id, &msg2).unwrap();

        // Alice decrypts Bob's message.
        let decrypted2 = alice.decrypt(&channel_id, &ciphertext2).unwrap();
        assert_eq!(decrypted2.sender, bob.identity_actor_id());
        if let ChannelMessageBody::Text(text) = &decrypted2.body {
            assert_eq!(text, "hello from bob");
        } else {
            panic!("expected Text body");
        }
    }

    #[test]
    fn decrypt_binds_sender_to_authenticated_leaf() {
        // Regression pin: `decrypt` must return the
        // sender of the MLS-*authenticated* leaf, NOT the self-asserted `sender`
        // the author wrote into the encrypted payload. An in-group member who
        // forges the inner `sender` must not be able to impersonate another
        // member — which would also bypass the documented sender-only-delete
        // floor (`conversations.md` § Reactions & message delete).
        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();
        let (charlie, _tmp_c) = make_engine();

        // A 3-member group: alice creates with both bob and charlie in one
        // create_group; both join from the shared Welcome.
        let mut kps = bob.generate_key_packages(1).unwrap();
        kps.extend(charlie.generate_key_packages(1).unwrap());
        let (channel_id, welcome) = alice.create_group(&kps).unwrap();
        let welcome_bytes = welcome.to_bytes().unwrap();
        bob.join_from_welcome_bytes(&welcome_bytes).unwrap();
        charlie.join_from_welcome_bytes(&welcome_bytes).unwrap();

        // Mallory == alice (a real, authenticated member) seals a message whose
        // inner `sender` is forged to claim it came from charlie (the victim).
        let forged = ChannelMessage {
            sender: charlie.identity_actor_id(), // forged victim id
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("i am totally charlie".into()),
            timestamp: Timestamp::now(),
        };
        let ciphertext = alice.encrypt(&channel_id, &forged).unwrap();

        // Bob decrypts. The returned sender MUST be the authenticated leaf
        // (alice), never the forged victim (charlie).
        let decrypted = bob.decrypt(&channel_id, &ciphertext).unwrap();
        assert_eq!(
            decrypted.sender,
            alice.identity_actor_id(),
            "decrypt must bind sender to the authenticated MLS leaf"
        );
        assert_ne!(
            decrypted.sender,
            charlie.identity_actor_id(),
            "the forged victim id must not survive decryption"
        );
    }

    #[test]
    fn scheduling_delivery_from_ephemeral_engine_preserves_imip_body() {
        // CalDAV ephemeral-sender invariant — the MLS-1 sender-bind must NOT
        // break it. The auto-schedule gateway seals an iMIP from a one-off
        // *ephemeral* engine whose leaf is NOT the organizer, stamping
        // `sender = <real organizer>`. After the bind, `decrypt` returns the
        // *ephemeral* leaf id (not the organizer)
        // — sound because the scheduling receive path re-authenticates via the
        // iMIP `ORGANIZER` and never reads `cm.sender` (`fauna_mls.rs`
        // `poll_inbound_scheduling` reads only `cm.body`). What must survive is
        // the iMIP BODY, carried verbatim.
        let (ephemeral, _tmp_e) = make_engine(); // the one-off gateway engine
        let (recipient, _tmp_r) = make_engine();
        let organizer = ActorKeypair::generate().actor_id(); // != the ephemeral leaf

        let recipient_kp = recipient.generate_key_packages_bytes(1).unwrap()[0].clone();
        let imip: Vec<u8> = b"BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n".to_vec();

        let delivery = ephemeral
            .build_scheduling_delivery(&recipient_kp, organizer, imip.clone())
            .unwrap();

        // The recipient joins the one-off group and processes the application
        // envelope back to the iMIP.
        recipient
            .join_from_welcome_bytes(&delivery.welcome_bytes)
            .unwrap();
        let env: ChannelEnvelope = fauna_cbor::decode_strict(&delivery.app_envelope).unwrap();
        let ChannelEnvelope::Application(ct) = env else {
            panic!("expected an application envelope");
        };
        let cm = recipient.decrypt(&delivery.channel_id, &ct).unwrap();

        // The iMIP body is carried verbatim — the scheduling content survives the
        // sender bind untouched.
        match &cm.body {
            ChannelMessageBody::Scheduling(bytes) => assert_eq!(bytes, &imip),
            other => panic!("expected a Scheduling body, got {other:?}"),
        }
        // The bound sender is now the authenticated *ephemeral* leaf, NOT the
        // organizer — proving the receive path must not (and does not) trust
        // `cm.sender` for scheduling.
        assert_eq!(cm.sender, ephemeral.identity_actor_id());
        assert_ne!(cm.sender, organizer);
    }

    // ── MLS-2: leaf credential ↔ signature-key binding ──────────────────────
    // (security review tracked internally, § Part 2). MLS authenticates only
    // the *signature*; the leaf BasicCredential
    // (the 32-byte ActorId) is opaque to MLS. A patched in-group client can present
    // a leaf whose credential names a victim while signing with its own key, so the
    // MLS-1 sender-bind (which trusts the credential) attributes the attacker's
    // messages to the victim — re-opening impersonation + the sender-only-delete
    // floor one layer down. The fix enforces credential == leaf signature key at
    // every leaf admission point.

    // The CONFIRM-FIRST exploit was witnessed on baseline before this fix: with
    // no binding check, the forged-credential leaf was admitted and `decrypt`
    // returned the FORGED victim id (impersonation). The three regressions below
    // assert the secure behavior permanently — each was RED on baseline.

    #[test]
    fn mls2_forged_key_package_rejected_at_add_and_create_group() {
        // Add vector: an attacker uploads a KeyPackage claiming Carol (signed by
        // her own key); an honest member who fetches + adds it must reject it.
        let carol = ActorKeypair::generate().actor_id();
        let (alice, _tmp_a) = make_engine();
        let (mallory, _tmp_m) = make_engine();
        let forged_bytes = mallory.forge_key_package_bytes_for_test(carol).unwrap();

        // The forged KP is a structurally valid KeyPackage (openmls self-sig is
        // by Mallory's key) — `key_package_from_bytes` accepts it; the binding
        // check fires only at admission.
        let forged_kp = alice.key_package_from_bytes(&forged_bytes).unwrap();

        // create_group with a forged member KP is rejected.
        let create_res = alice.create_group(std::slice::from_ref(&forged_kp));
        assert!(
            matches!(create_res, Err(MlsError::CredentialBindingViolation(_))),
            "create_group must reject a forged-credential member KeyPackage"
        );

        // add_member onto an existing honest group is rejected too.
        let (bob, _tmp_b) = make_engine();
        let bob_kp = bob.generate_key_packages(1).unwrap();
        let (channel_id, _welcome) = alice.create_group(&bob_kp).unwrap();
        let add_res = alice.add_member_from_bytes(&channel_id, &forged_bytes);
        assert!(
            matches!(add_res, Err(MlsError::CredentialBindingViolation(_))),
            "add_member must reject a forged-credential KeyPackage"
        );
    }

    #[test]
    fn mls2_forged_leaf_rejected_on_join_from_welcome() {
        // Malicious-inviter vector: a patched client whose OWN leaf claims Carol
        // invites an honest member. The honest member learns the forged leaf from
        // the ratchet tree in the Welcome and must reject the join, not trust a
        // roster that names Carol for a leaf Mallory controls.
        let carol = ActorKeypair::generate().actor_id();
        let mallory =
            MlsEngine::new_in_memory_forged_for_test(ActorKeypair::generate(), carol).unwrap();
        let (bob, _tmp_b) = make_engine();

        let bob_kp = bob.generate_key_packages(1).unwrap();
        let (_channel_id, welcome) = mallory.create_group(&bob_kp).unwrap();
        let welcome_bytes = welcome.to_bytes().unwrap();

        let join_res = bob.join_from_welcome_bytes(&welcome_bytes);
        assert!(
            matches!(join_res, Err(MlsError::CredentialBindingViolation(_))),
            "join must reject a Welcome whose ratchet tree carries a forged-credential leaf"
        );
    }

    #[test]
    fn mls2_forged_leaf_rejected_in_process_commit() {
        // Post-join vector: an honest member is already in a group; a co-member's
        // patched client later commits an Add of a forged-credential leaf. The
        // honest receiver's process_commit must reject the commit (not merge a
        // poisoned roster). `mallory` joins honestly, then emits the malicious
        // commit via the unchecked test seam (simulating the patched client).
        let carol = ActorKeypair::generate().actor_id();
        let (mallory, _tmp_m) = make_engine();
        let (bob, _tmp_b) = make_engine();

        // Honest 2-member group: mallory creates with bob.
        let bob_kp = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = mallory.create_group(&bob_kp).unwrap();
        bob.join_from_welcome_bytes(&welcome.to_bytes().unwrap())
            .unwrap();

        // A forged KP claiming Carol (signed by dave's key).
        let (dave, _tmp_d) = make_engine();
        let forged_kp = dave.forge_key_package_bytes_for_test(carol).unwrap();

        // Mallory's patched client adds it WITHOUT the binding check → a commit.
        let (commit_bytes, _welcome) = mallory
            .add_member_from_bytes_unchecked_for_test(&channel_id, &forged_kp)
            .unwrap();

        let commit_res = bob.process_commit(&channel_id, &commit_bytes);
        assert!(
            matches!(commit_res, Err(MlsError::CredentialBindingViolation(_))),
            "process_commit must reject a commit that introduces a forged-credential leaf"
        );
    }

    #[test]
    fn mls2_verify_uploaded_key_package_enforces_uploader_binding() {
        // Nest-side defense-in-depth: an honest KeyPackage uploaded under its own
        // actor passes; a forged-credential KP, an honest KP uploaded under the
        // wrong actor, and malformed bytes are all rejected.
        let (alice, _tmp_a) = make_engine();
        let alice_id = alice.identity_actor_id().0;
        let honest = alice.generate_key_packages_bytes(1).unwrap()[0].clone();

        // Honest KP under the real uploader → accepted.
        assert!(verify_uploaded_key_package(&honest, &alice_id).is_ok());

        // Forged credential (claims the victim, signed by Alice) → rejected, even
        // if uploaded as the victim (credential ≠ leaf signature key).
        let victim = ActorKeypair::generate().actor_id();
        let forged = alice.forge_key_package_bytes_for_test(victim).unwrap();
        assert!(matches!(
            verify_uploaded_key_package(&forged, &alice_id),
            Err(MlsError::CredentialBindingViolation(_))
        ));
        assert!(matches!(
            verify_uploaded_key_package(&forged, &victim.0),
            Err(MlsError::CredentialBindingViolation(_))
        ));

        // Honest KP, but uploaded under a different actor → rejected.
        let other = ActorKeypair::generate().actor_id().0;
        assert!(matches!(
            verify_uploaded_key_package(&honest, &other),
            Err(MlsError::CredentialBindingViolation(_))
        ));

        // Garbage bytes → rejected (not a valid KeyPackage).
        assert!(verify_uploaded_key_package(&[0xde, 0xad, 0xbe, 0xef], &alice_id).is_err());
    }

    #[test]
    fn add_and_remove_member() {
        let (alice, _tmp_a) = make_engine();
        let (bob, _tmp_b) = make_engine();
        let (charlie, _tmp_c) = make_engine();

        // Alice creates a group with Bob.
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _bob_channel_id = bob.join_from_welcome(welcome).unwrap();

        // Alice adds Charlie.
        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (commit_bytes, charlie_welcome) =
            alice.add_member(&channel_id, &charlie_kps[0]).unwrap();

        // Bob processes the add commit.
        bob.process_commit(&channel_id, &commit_bytes).unwrap();

        // Charlie joins.
        let charlie_channel_id = charlie.join_from_welcome(charlie_welcome).unwrap();
        assert_eq!(channel_id, charlie_channel_id);

        // Alice removes Bob (leaf index 1).
        let remove_commit_bytes = alice.remove_member(&channel_id, 1).unwrap();

        // Charlie processes the remove commit.
        charlie
            .process_commit(&channel_id, &remove_commit_bytes)
            .unwrap();
    }

    /// `ui/feed.md` ruling 5's residue, pinned at the engine (the one place a
    /// device learns of its own removal): a removed member's device that has
    /// **processed** its removal keeps the group loaded but evicted — it can
    /// export no secret for the post-removal epoch, so it can seal no new
    /// room-restricted post — while the room-post secret it kept for the epoch
    /// it was removed at still answers, on its own device and on every
    /// remaining member's, with no expiry (ruling 6: a rotated-out member keeps
    /// what it held, and so do those who shared the epoch with it). The seam's
    /// `room_post_rooms` drops the evicted room on
    /// [`MlsEngine::is_group_active`]; what this cannot reach — a device that
    /// never processed the commit — is the residue the ruling states.
    #[test]
    fn a_removed_member_keeps_the_epoch_it_left_but_seals_nothing_new() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel, welcome) = alice.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();

        let (epoch_before, secret_before) = bob.export_room_post_secret(&channel).unwrap();
        assert_eq!(
            alice.export_room_post_secret(&channel).unwrap(),
            (epoch_before, secret_before),
            "two members at one epoch export one room-post secret"
        );
        assert!(bob.is_group_active(&channel), "a seated member is active");

        // Alice removes bob (leaf 1 — he joined second); bob processes his own
        // removal, the ordinary end of a membership.
        let commit = alice.remove_member(&channel, 1).unwrap();
        bob.process_commit(&channel, &commit).unwrap();

        assert!(
            bob.has_group(&channel),
            "the evicted group stays loaded — the state a removed user's snapshot carries"
        );
        assert!(
            !bob.is_group_active(&channel),
            "…but it is no longer active: the seat is gone"
        );
        assert!(
            alice.is_group_active(&channel),
            "the remaining member's seat is not"
        );
        let refused = bob
            .export_room_post_secret(&channel)
            .expect_err("an evicted group exports no secret for the epoch that removed it");
        assert!(
            refused.to_string().contains("UseAfterEviction"),
            "refused as an eviction, not some other failure: {refused}"
        );

        // What was held stays held — on both sides, with no freshness check.
        assert_eq!(
            bob.room_post_secret_at(&channel, epoch_before).unwrap(),
            secret_before,
            "the removed member keeps the secret of the epoch it was removed at"
        );
        assert_eq!(
            alice.room_post_secret_at(&channel, epoch_before).unwrap(),
            secret_before,
            "…and so does the member who removed it, for as long as it keeps the group"
        );
        assert_ne!(
            alice.export_room_post_secret(&channel).unwrap().1,
            secret_before,
            "the post-removal epoch has a secret of its own, which bob never derives"
        );
    }

    /// The device-owned-epoch **rebase mechanism** (slice 3,
    /// `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync): a
    /// self-`Update` commit that loses the commit race is *cleared*, the winning
    /// commit is processed, and a fresh self-update rebased onto the new epoch
    /// merges convergently — after which both members exchange application
    /// traffic. Proves the new primitives: `self_update` stages a commit without
    /// merging (epoch unchanged until merge), `clear_pending_commit` discards a
    /// losing commit (so a subsequent `self_update` is not blocked by a lingering
    /// pending), and `merge_pending_commit` finalizes the rebased commit. The same
    /// clear→process→retry path serves the same-actor cross-device race the gate
    /// enforces; here two distinct members stand in for two of a user's devices.
    #[test]
    fn stale_self_update_clears_rebases_and_converges() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();

        // Alice creates a group with Bob; both merge to the same epoch.
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();
        let epoch0 = alice.current_epoch(&channel_id).unwrap();
        assert_eq!(epoch0, bob.current_epoch(&channel_id).unwrap());

        // Both devices build a self-update commit from the SAME epoch.
        let alice_commit = alice.self_update(&channel_id).unwrap();
        let _bob_commit = bob.self_update(&channel_id).unwrap();

        // self_update STAGES without merging — neither epoch has advanced yet.
        assert_eq!(
            alice.current_epoch(&channel_id).unwrap(),
            epoch0,
            "self_update stages a pending commit but does not merge it"
        );

        // Bob's commit lands first (nest-ordered); he merges his own. Alice loses.
        bob.merge_pending_commit(&channel_id).unwrap();
        let bob_commit = _bob_commit;

        // Alice rebases: clear her losing pending, process Bob's, re-self-update,
        // merge. `alice_commit` is now dead (never sent).
        let _ = alice_commit;
        alice.clear_pending_commit(&channel_id).unwrap();
        alice.process_commit(&channel_id, &bob_commit).unwrap();
        let alice_rebased = alice.self_update(&channel_id).unwrap();
        alice.merge_pending_commit(&channel_id).unwrap();
        bob.process_commit(&channel_id, &alice_rebased).unwrap();

        // Converged: epochs match and application traffic flows Alice → Bob.
        assert_eq!(
            alice.current_epoch(&channel_id).unwrap(),
            bob.current_epoch(&channel_id).unwrap(),
            "both members converge on the rebased epoch"
        );
        let msg = ChannelMessage {
            sender: alice.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("after rebase".into()),
            timestamp: Timestamp::now(),
        };
        let ciphertext = alice.encrypt(&channel_id, &msg).unwrap();
        let got = bob.decrypt(&channel_id, &ciphertext).unwrap();
        assert!(
            matches!(got.body, ChannelMessageBody::Text(ref t) if t == "after rebase"),
            "peer decrypts application traffic on the rebased epoch"
        );
    }

    /// `clear_pending_commit` is a safe no-op when the group has no pending commit
    /// (openmls returns early on `Operational`), so the rebase loop can call it
    /// unconditionally on a `stale` rejection without first checking for a pending.
    #[test]
    fn clear_pending_commit_is_noop_without_a_pending() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, _welcome) = alice.create_group(&bob_kps).unwrap();
        let epoch = alice.current_epoch(&channel_id).unwrap();
        // No staged commit: clearing is a no-op and leaves the epoch untouched.
        alice.clear_pending_commit(&channel_id).unwrap();
        assert_eq!(alice.current_epoch(&channel_id).unwrap(), epoch);
    }

    /// The staged twins of `add_member`/`remove_member` return the same commit
    /// bytes but leave the group at its prior epoch until `merge_pending_commit`
    /// — the gate-send seam relies on the commit existing before the epoch moves.
    #[test]
    fn add_member_staged_defers_the_epoch_until_merge() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let (charlie, _c) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();
        let epoch = alice.current_epoch(&channel_id).unwrap();

        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (commit_bytes, charlie_welcome) = alice
            .add_member_staged(&channel_id, &charlie_kps[0])
            .unwrap();
        assert_eq!(
            alice.current_epoch(&channel_id).unwrap(),
            epoch,
            "add_member_staged does not advance the epoch before merge"
        );

        // Merge-on-accept: the epoch advances and the receivers process the same
        // commit bytes that were staged, converging.
        alice.merge_pending_commit(&channel_id).unwrap();
        assert!(alice.current_epoch(&channel_id).unwrap() > epoch);
        bob.process_commit(&channel_id, &commit_bytes).unwrap();
        charlie.join_from_welcome(charlie_welcome).unwrap();
        assert_eq!(
            bob.current_epoch(&channel_id).unwrap(),
            alice.current_epoch(&channel_id).unwrap()
        );
    }

    /// `add_member_staged_from_bytes` — the from-bytes twin the gated conversation
    /// add path drives: like [`MlsEngine::add_member_staged`] it stages the commit
    /// without advancing the epoch, and (like the optimistic `add_member_from_bytes`)
    /// it returns the Welcome as *bytes* the newcomer joins from directly.
    /// The room verdict's own guard behind property 3: the honest context names the policy's extension type in its
    /// `required_capabilities`, and a context that keeps the policy but drops
    /// the requirement — what a client patched past openMLS's own
    /// GroupContextExtensions validation would propose — reads as
    /// unrequired, which `staged_commit_facts` turns into the same refusal a
    /// dropped policy gets.
    #[test]
    fn a_context_that_drops_the_policys_requirement_reads_as_unrequired() {
        use crate::room_policy::RoomPolicy;
        let (alice, _a) = make_engine();
        let policy = RoomPolicyExtension::new(
            alice
                .sign_room_policy(&RoomPolicy::initial(alice.identity_actor_id(), None))
                .unwrap(),
        );
        let honest = room_context_extensions(&policy).unwrap();
        assert!(requires_room_policy(&honest));

        let stripped = Extensions::from_vec(vec![Extension::Unknown(
            ROOM_POLICY_EXTENSION_TYPE,
            UnknownExtension(policy.to_bytes().unwrap()),
        )])
        .unwrap();
        assert!(!requires_room_policy(&stripped));

        let emptied = Extensions::from_vec(vec![
            Extension::Unknown(
                ROOM_POLICY_EXTENSION_TYPE,
                UnknownExtension(policy.to_bytes().unwrap()),
            ),
            Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(&[], &[], &[])),
        ])
        .unwrap();
        assert!(
            !requires_room_policy(&emptied),
            "an empty requirement names nothing"
        );

        // The extension set's validator type is the group-config builder's
        // to name; feeding each set to that one door fixes it by inference.
        for ext in [honest, stripped, emptied] {
            let _ = MlsGroupCreateConfig::builder().with_group_context_extensions(ext);
        }
    }

    #[test]
    fn add_member_staged_from_bytes_defers_epoch_and_returns_welcome_bytes() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let (charlie, _c) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();
        let epoch = alice.current_epoch(&channel_id).unwrap();

        let charlie_kp_bytes = charlie.generate_key_packages_bytes(1).unwrap().remove(0);
        let (commit_bytes, welcome_bytes) = alice
            .add_member_staged_from_bytes(&channel_id, &charlie_kp_bytes)
            .unwrap();
        assert_eq!(
            alice.current_epoch(&channel_id).unwrap(),
            epoch,
            "staged-from-bytes does not advance the epoch before merge"
        );

        // Merge-on-accept converges bob (via the staged commit bytes) and charlie
        // (via the returned Welcome bytes) onto alice's new epoch.
        alice.merge_pending_commit(&channel_id).unwrap();
        bob.process_commit(&channel_id, &commit_bytes).unwrap();
        let charlie_channel = charlie.join_from_welcome_bytes(&welcome_bytes).unwrap();
        assert_eq!(
            charlie_channel, channel_id,
            "the returned Welcome bytes join the same channel"
        );
        assert_eq!(
            bob.current_epoch(&channel_id).unwrap(),
            alice.current_epoch(&channel_id).unwrap()
        );
    }

    /// `forget_group` drops the group from `has_group` (the shared-folder leave
    /// primitive) and is an idempotent no-op for an unknown channel.
    #[test]
    fn forget_group_drops_from_has_group() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, _welcome) = alice.create_group(&bob_kps).unwrap();
        assert!(alice.has_group(&channel_id), "created group is present");

        alice.forget_group(&channel_id).unwrap();
        assert!(!alice.has_group(&channel_id), "forgotten group is gone");

        // Idempotent: forgetting again (and an unknown channel) is a no-op.
        alice.forget_group(&channel_id).unwrap();
        alice
            .forget_group(&ChannelId::from_group_id(&[0x9fu8; 20]))
            .unwrap();
    }

    /// A forgotten group stays forgotten across an engine restart — the durable
    /// `active_groups` row is dropped, so `new()` does not reload it (else the
    /// leaver's `has_group` filter would flip back true and the set would reappear).
    #[test]
    fn forget_group_survives_reload() {
        let tmp = NamedTempFile::new().unwrap();
        // A fixed secret so both opens rebuild the *same* identity (the real restart).
        let secret = [0x5eu8; 32];
        let (bob, _b) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();

        let channel_id = {
            let engine = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
            let (channel_id, _welcome) = engine.create_group(&bob_kps).unwrap();
            assert!(engine.has_group(&channel_id));
            engine.forget_group(&channel_id).unwrap();
            channel_id
        };

        // Reopen the same identity over the same db — the forgotten group must not
        // reload.
        let reopened = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
        assert!(
            !reopened.has_group(&channel_id),
            "a forgotten group does not reload after restart"
        );
    }

    /// **An `active_groups` row naming a group the provider snapshot holds no
    /// byte of is swept at open, not carried for ever.** The pre-fix launch
    /// swap left exactly this leftover (a listed group whose entries the swap
    /// wiped, flushed back by `retire()`); `new()` warned and dropped the group
    /// but kept the row, so every later open warned again over a group that
    /// could never load.
    #[test]
    fn a_stale_active_groups_row_is_swept_at_open() {
        let tmp = NamedTempFile::new().unwrap();
        let secret = [0x5fu8; 32];
        let (bob, _b) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let channel_id = {
            let engine = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
            let (channel_id, _welcome) = engine.create_group(&bob_kps).unwrap();
            // The leftover: the row stands, the snapshot holds nothing for it.
            engine.provider.storage().values.write().unwrap().clear();
            engine.save_state().unwrap();
            channel_id
        };

        let reopened = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
        assert!(
            !reopened.has_group(&channel_id),
            "a group with no state does not load"
        );
        assert!(
            reopened.storage.list_active_groups().unwrap().is_empty(),
            "the stale row is swept at open"
        );
    }

    /// The pending-commit identity ([`MlsEngine::pending_commit_hash`]) tracks the
    /// group's pending exactly: it appears when a commit is staged, equals
    /// `blake3` of the returned wire bytes, and vanishes on merge and on clear.
    /// This is the identity a byted resume checks before merging, so its lifecycle
    /// must never drift from `has_pending_commit`.
    #[test]
    fn pending_commit_hash_tracks_the_staged_commit() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let (charlie, _c) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, _welcome) = alice.create_group(&bob_kps).unwrap();

        // No pending yet.
        assert!(!alice.has_pending_commit(&channel_id));
        assert_eq!(alice.pending_commit_hash(&channel_id), None);

        // Stage a Remove (Bob is leaf 1) — the stamp equals blake3 of the bytes.
        let commit = alice.remove_member_staged(&channel_id, 1).unwrap();
        assert!(alice.has_pending_commit(&channel_id));
        assert_eq!(
            alice.pending_commit_hash(&channel_id),
            Some(*blake3::hash(&commit).as_bytes()),
        );

        // Merge drops it.
        alice.merge_pending_commit(&channel_id).unwrap();
        assert!(!alice.has_pending_commit(&channel_id));
        assert_eq!(alice.pending_commit_hash(&channel_id), None);

        // And a staged-then-cleared commit leaves none behind. (Add Charlie so a
        // fresh Remove has a target.)
        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (_c, _w) = alice.add_member(&channel_id, &charlie_kps[0]).unwrap();
        let _staged = alice.remove_member_staged(&channel_id, 1).unwrap();
        assert!(alice.pending_commit_hash(&channel_id).is_some());
        alice.clear_pending_commit(&channel_id).unwrap();
        assert_eq!(alice.pending_commit_hash(&channel_id), None);
    }

    /// The identity is durable: it rides the provider KV, so a native
    /// `save_state` → reload restores `{pending, its identity}` together. This is
    /// a load-bearing crash-safety property: a resumed engine that still
    /// holds a staged pending also still knows *which* commit it is.
    #[test]
    fn pending_commit_hash_survives_reload() {
        let tmp = NamedTempFile::new().unwrap();
        let secret = [0x71u8; 32];
        let (bob, _b) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();

        let (channel_id, commit) = {
            let engine = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
            let (channel_id, _welcome) = engine.create_group(&bob_kps).unwrap();
            let commit = engine.remove_member_staged(&channel_id, 1).unwrap();
            engine.save_state().unwrap(); // snapshot the KV with the staged pending
            (channel_id, commit)
        };

        let reopened = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
        assert!(
            reopened.has_pending_commit(&channel_id),
            "the staged pending reloads from the snapshot",
        );
        assert_eq!(
            reopened.pending_commit_hash(&channel_id),
            Some(*blake3::hash(&commit).as_bytes()),
            "and its identity reloads with it — never a pending without its stamp",
        );
    }

    #[test]
    fn encrypt_envelope_roundtrip() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        let msg = ChannelMessage {
            sender: alice.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("envelope test".into()),
            timestamp: Timestamp::now(),
        };

        let envelope_bytes = alice.encrypt_to_envelope(&channel_id, &msg).unwrap();
        let env: ChannelEnvelope = fauna_cbor::decode_strict(&envelope_bytes).unwrap();
        let ChannelEnvelope::Application(ct) = env else {
            panic!("expected an application envelope");
        };
        let decoded = bob.decrypt(&channel_id, &ct).unwrap();
        assert!(matches!(&decoded.body, ChannelMessageBody::Text(t) if t == "envelope test"));
    }

    /// The `process_commit` error taxonomy (Rule 2, `devices.md` § Cross-device
    /// MLS group-state sync): bytes no member can ever apply classify as
    /// `InvalidCommit` (an ingest cursor may advance past them — stalling would
    /// hand any in-group member a remote DoS), while a missing local group is a
    /// *local* condition (`ChannelNotFound`) a cursor must stop on.
    #[test]
    fn process_commit_classifies_intrinsic_invalid_vs_local() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        // Undeserializable garbage — invalid for every member.
        let err = bob
            .process_commit(&channel_id, b"not an mls message")
            .unwrap_err();
        assert!(
            matches!(err, MlsError::InvalidCommit(_)),
            "garbage bytes: {err:?}"
        );

        // A well-formed MLS message that decrypts to an *application* body on
        // the commit rail — every member sees the same non-commit.
        let msg = ChannelMessage {
            sender: alice.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("not a commit".into()),
            timestamp: Timestamp::now(),
        };
        let app_ct = alice.encrypt(&channel_id, &msg).unwrap();
        let err = bob.process_commit(&channel_id, &app_ct).unwrap_err();
        assert!(
            matches!(err, MlsError::InvalidCommit(_)),
            "application body on the commit rail: {err:?}"
        );
        // An intrinsically-invalid verdict permits no retry (the cursor
        // advances), so the consumption-window memo must NOT survive it — a
        // leftover entry would stall a Rule-2 heal re-walk of the same record.
        assert!(
            bob.merge_failed_commit_memo(&channel_id).is_none(),
            "a skipped record leaves no memo behind"
        );

        // Well-formed MLS bytes against a channel this engine has no group for
        // are a LOCAL condition (the group may exist and advance without us —
        // e.g. the Welcome hasn't arrived yet), never `InvalidCommit`.
        // (Malformed bytes classify invalid even here — deserialization is
        // deterministic on the bytes alone, so it runs first.)
        let unknown = ChannelId::from_group_id(b"missing-group");
        let err = bob.process_commit(&unknown, &app_ct).unwrap_err();
        assert!(matches!(err, MlsError::ChannelNotFound(_)), "{err:?}");
    }

    /// Staging a commit's update path needs the receiver's own PRIVATE epoch
    /// decryption keypairs — device-local state outside the group's canonical
    /// shared view — so "reached deeper validation ⇒ same verdict on every
    /// member" is FALSE. Delete only this device's own epoch keypairs (the
    /// bytes untouched — the committer merged them and the welcomed member
    /// joins by them) and `process_commit` must classify the failure *local*
    /// (cursor stalls before the commit), never `InvalidCommit` (cursor
    /// advances past a transition the group applied → every later message
    /// sealed under the new epoch is silently dropped, user-irrecoverable).
    #[test]
    fn own_epoch_key_absence_classifies_local_not_intrinsically_invalid() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let (charlie, _c) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        // A genuine membership commit carrying an update path, valid for the
        // group: Alice (the committer) holds it merged, and Charlie joins by
        // its Welcome — the bytes are appliable by every member with intact
        // local key material.
        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (commit_bytes, welcome_c) = alice.add_member(&channel_id, &charlie_kps[0]).unwrap();
        let _ = charlie.join_from_welcome(welcome_c).unwrap();

        // The only delta on Bob: his own private epoch decryption keypairs are
        // gone (torn/stale provider snapshot, replica restore, storage read
        // error — all real production shapes).
        let epoch_before = bob.current_epoch(&channel_id).unwrap();
        bob.delete_own_epoch_keypairs_for_test(&channel_id).unwrap();

        let err = bob.process_commit(&channel_id, &commit_bytes).unwrap_err();
        assert!(
            !matches!(err, MlsError::InvalidCommit(_)),
            "a purely local key absence must never classify as intrinsically \
             invalid (Skipped → the cursor eats a commit the group applied): {err:?}"
        );
        assert!(
            matches!(err, MlsError::OpenMls(_)),
            "local key absence classifies as the local failure it is: {err:?}"
        );
        assert_eq!(
            bob.current_epoch(&channel_id).unwrap(),
            epoch_before,
            "Bob's epoch is provably unchanged — the group advanced without him"
        );

        // The decrypt above consumed the sender-ratchet generation
        // before staging failed, so the memo must be armed by the STAGE
        // failure (not only by a merge failure) — without it the retry would
        // re-decrypt, hit the consumed generation, and misclassify as
        // intrinsically invalid, re-opening the loss one pass later.
        assert!(
            bob.merge_failed_commit_memo(&channel_id).is_some(),
            "a stage-local failure arms the memo at the consumption point"
        );
        let err = bob.process_commit(&channel_id, &commit_bytes).unwrap_err();
        assert!(
            matches!(err, MlsError::OpenMls(_)),
            "the retry keeps reporting the same local failure: {err:?}"
        );
        assert_eq!(bob.current_epoch(&channel_id).unwrap(), epoch_before);
    }

    /// The decrypt layer's one device-local **consuming** error must round
    /// local ⇒ stall, exactly like the staging layer's `MissingDecryptionKey`
    /// above. openMLS consumes the sender-ratchet generation before the content
    /// AEAD, so an in-group member who burns a victim's next handshake
    /// generation with a forged-`sender_data` garbage message (AeadError →
    /// Skipped, generation consumed) makes the victim's own real commit at that
    /// generation return `SecretReuseError`. Rounding that to `InvalidCommit`
    /// (Skipped) eats a commit the group applied ⇒ silent, attacker-triggerable
    /// loss (permanent for a single-device victim). The forged handshake
    /// message cannot be crafted in-process (openMLS does not expose
    /// `sender_data` construction), so the mapping is exercised directly via
    /// `classify_decrypt_error`.
    #[test]
    fn secret_reuse_error_classifies_local_stall_not_intrinsically_invalid() {
        // The two device-local ratchet errors round local ⇒ stall, memo kept.
        // `SecretReuseError` is the load-bearing one (it *consumes*);
        // `RatchetTypeError` is an internal-invariant local failure.
        for st in [
            SecretTreeError::SecretReuseError,
            SecretTreeError::RatchetTypeError,
        ] {
            let label = format!("{st:?}");
            let (err, keep_memo) =
                classify_decrypt_error(MessageDecryptionError::SecretTreeError(st));
            assert!(
                matches!(err, MlsError::OpenMls(_)),
                "{label} is a device-local ratchet failure — it must stall \
                 (local), never Skip (the cursor would eat a commit the group \
                 applied): {err:?}"
            );
            assert!(
                keep_memo,
                "{label}: a consuming local failure keeps the memo"
            );
        }

        // The library/provider/codec carve-out inside the secret tree stays
        // local (unchanged by the split).
        let (err, keep_memo) = classify_decrypt_error(MessageDecryptionError::SecretTreeError(
            SecretTreeError::LibraryError,
        ));
        assert!(
            matches!(err, MlsError::OpenMls(_)),
            "SecretTreeError::LibraryError is local: {err:?}"
        );
        assert!(keep_memo);

        // Regression: the deterministic wild-generation / bounds / ciphertext
        // failures stay Skipped — they are computed on the bytes at this
        // epoch's shared ratchet, so every honest member reaches the same
        // verdict, and stalling them would hand an in-group member a cheap
        // wild-generation DoS.
        for d in [
            MessageDecryptionError::SecretTreeError(SecretTreeError::TooDistantInThePast),
            MessageDecryptionError::SecretTreeError(SecretTreeError::TooDistantInTheFuture),
            MessageDecryptionError::SecretTreeError(SecretTreeError::IndexOutOfBounds),
            MessageDecryptionError::SecretTreeError(SecretTreeError::RatchetTooLong),
            MessageDecryptionError::GenerationOutOfBound,
            MessageDecryptionError::AeadError,
            MessageDecryptionError::WrongWireFormat,
            MessageDecryptionError::MalformedContent,
        ] {
            let label = format!("{d:?}");
            let (err, keep_memo) = classify_decrypt_error(d);
            assert!(
                matches!(err, MlsError::InvalidCommit(_)),
                "{label} is deterministic on the bytes — it must stay Skipped: {err:?}"
            );
            assert!(!keep_memo, "{label}: a Skipped verdict drops the memo");
        }
    }

    /// The memo must survive a relaunch whose restored provider snapshot
    /// post-dates the consumption (`save_state` runs on many
    /// ordinary paths — any group create/join, gate-less add/remove, resumed-
    /// pending clear — so "a relaunch restores the pre-consumption snapshot"
    /// is not a premise the retry may rest on). The memo lives in the provider
    /// KV, so the *same* snapshot that persists the consumed generation
    /// persists the memo beside it: a torn `{consumed-provider, no-memo}`
    /// relaunch — the shape whose retry re-decrypts, hits the consumed
    /// generation, and misclassifies as intrinsically invalid — is
    /// unrepresentable.
    #[test]
    fn stage_failure_memo_survives_relaunch_onto_a_post_consumption_snapshot() {
        let tmp = NamedTempFile::new().unwrap();
        let secret = [0x72u8; 32];
        let (alice, _a) = make_engine();
        let (charlie, _c) = make_engine();

        let (channel_id, commit_bytes) = {
            let bob = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
            let bob_kps = bob.generate_key_packages(1).unwrap();
            let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
            let _ = bob.join_from_welcome(welcome).unwrap();

            let charlie_kps = charlie.generate_key_packages(1).unwrap();
            let (commit_bytes, _welcome_c) =
                alice.add_member(&channel_id, &charlie_kps[0]).unwrap();

            // The stage-local failure: decrypt consumes the generation, the
            // update path fails on Bob's missing own keypairs, the memo arms.
            bob.delete_own_epoch_keypairs_for_test(&channel_id).unwrap();
            let err = bob.process_commit(&channel_id, &commit_bytes).unwrap_err();
            assert!(matches!(err, MlsError::OpenMls(_)), "{err:?}");

            // Ordinary later activity snapshots the WHOLE provider — consumed
            // generation included. This is the post-consumption snapshot the
            // next launch will restore.
            bob.save_state().unwrap();
            (channel_id, commit_bytes)
        };

        // The relaunch. Its provider already consumed the commit's ratchet
        // generation, so a memo-less retry would misreport the bytes as
        // intrinsically invalid and the cursor would eat the commit.
        let bob = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
        assert!(
            bob.merge_failed_commit_memo(&channel_id).is_some(),
            "the memo restores with the post-consumption snapshot it annotates"
        );
        let err = bob.process_commit(&channel_id, &commit_bytes).unwrap_err();
        assert!(
            !matches!(err, MlsError::InvalidCommit(_)),
            "the retry over the restored snapshot must not misclassify as \
             intrinsically invalid: {err:?}"
        );
        assert!(matches!(err, MlsError::OpenMls(_)), "{err:?}");
    }

    /// The same durability across the replica plane: the memo is part of the
    /// provider KV, so `ProviderReplica::from_engine` carries it and
    /// `restore_into` restores it — a wasm engine (no local SQLite by
    /// construction) and a cross-device restore get the identical guarantee.
    #[test]
    fn stage_failure_memo_rides_the_provider_replica() {
        use crate::state_replica::ProviderReplica;

        let secret = [0x73u8; 32];
        let (alice, _a) = make_engine();
        let (charlie, _c) = make_engine();

        let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (commit_bytes, _welcome_c) = alice.add_member(&channel_id, &charlie_kps[0]).unwrap();

        bob.delete_own_epoch_keypairs_for_test(&channel_id).unwrap();
        let err = bob.process_commit(&channel_id, &commit_bytes).unwrap_err();
        assert!(matches!(err, MlsError::OpenMls(_)), "{err:?}");

        let replica = ProviderReplica::from_engine(&bob);
        let restored = MlsEngine::new_in_memory(ActorKeypair::from_secret(secret)).unwrap();
        replica.restore_into_unchecked(&restored).unwrap();

        assert!(
            restored.merge_failed_commit_memo(&channel_id).is_some(),
            "the memo rides the replica's provider values"
        );
        let err = restored
            .process_commit(&channel_id, &commit_bytes)
            .unwrap_err();
        assert!(
            !matches!(err, MlsError::InvalidCommit(_)),
            "the restored engine's retry must not misclassify: {err:?}"
        );
    }

    /// The merge-failure memo: a commit that validated but failed its local
    /// merge is un-reprocessable in this process (decryption consumed the
    /// sender-ratchet generation), so the memo must keep the retry reporting
    /// the same *local* failure — never re-decrypting into a validation error
    /// that would misclassify as `InvalidCommit` and let the cursor advance.
    /// A stale entry (group since advanced) is dropped and ignored. The
    /// record-on-merge-failure leg lives in `process_commit` itself; the
    /// in-memory provider cannot be made to fail a merge here, so the memo is
    /// seeded directly.
    #[test]
    fn merge_failure_memo_keeps_retry_local_until_healed() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let (charlie, _c) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (commit_bytes, _welcome) = alice.add_member(&channel_id, &charlie_kps[0]).unwrap();

        let epoch_before = bob.current_epoch(&channel_id).unwrap();
        bob.record_merge_failed_commit(
            &channel_id,
            *blake3::hash(&commit_bytes).as_bytes(),
            epoch_before,
        );

        // The retry reports the local failure without touching the ratchet.
        let err = bob.process_commit(&channel_id, &commit_bytes).unwrap_err();
        assert!(
            matches!(err, MlsError::OpenMls(_)),
            "memo'd retry stays a local failure: {err:?}"
        );
        assert_eq!(bob.current_epoch(&channel_id).unwrap(), epoch_before);

        // Heal (a resync — or a relaunch onto a pre-consumption snapshot,
        // which carries no memo by construction; here the memo short-circuited
        // before any decrypt, so clearing it is the equivalent) — the same
        // bytes now apply for real.
        bob.forget_merge_failed_commit(&channel_id);
        bob.process_commit(&channel_id, &commit_bytes).unwrap();
        assert_eq!(bob.current_epoch(&channel_id).unwrap(), epoch_before + 1);
        assert!(
            bob.merge_failed_commit_memo(&channel_id).is_none(),
            "a merged commit leaves no memo behind"
        );

        // A stale memo (recorded at an epoch the group has left) is ignored:
        // the already-merged commit classifies normally as past-epoch.
        bob.record_merge_failed_commit(
            &channel_id,
            *blake3::hash(&commit_bytes).as_bytes(),
            epoch_before,
        );
        let err = bob.process_commit(&channel_id, &commit_bytes).unwrap_err();
        assert!(matches!(err, MlsError::PastEpochCommit), "{err:?}");
        assert!(
            bob.merge_failed_commit_memo(&channel_id).is_none(),
            "the stale entry is dropped"
        );
    }

    #[test]
    fn duplicate_and_gap_detection() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        // Alice sends seq 1
        let msg1 = ChannelMessage {
            sender: alice.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("first".into()),
            timestamp: Timestamp::now(),
        };
        let ct1 = alice.encrypt(&channel_id, &msg1).unwrap();
        let dec1 = bob.decrypt_with_ordering(&channel_id, &ct1).unwrap();
        assert!(matches!(dec1, OrderedDecryptResult::Deliver(_)));

        // Alice sends seq 3 (gap — seq 2 missing)
        let msg3 = ChannelMessage {
            sender: alice.identity_actor_id(),
            sequence: 3,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("third".into()),
            timestamp: Timestamp::now(),
        };
        let ct3 = alice.encrypt(&channel_id, &msg3).unwrap();
        let dec3 = bob.decrypt_with_ordering(&channel_id, &ct3).unwrap();
        assert!(matches!(dec3, OrderedDecryptResult::DeliverWithGap { .. }));
    }

    #[test]
    fn auto_sequence_on_encrypt() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        let ct1 = alice
            .encrypt_with_sequence(&channel_id, ChannelMessageBody::Text("first".into()))
            .unwrap();
        let ct2 = alice
            .encrypt_with_sequence(&channel_id, ChannelMessageBody::Text("second".into()))
            .unwrap();

        let dec1 = bob.decrypt(&channel_id, &ct1).unwrap();
        let dec2 = bob.decrypt(&channel_id, &ct2).unwrap();
        assert_eq!(dec1.sequence, 1);
        assert_eq!(dec2.sequence, 2);
    }

    #[test]
    fn commit_envelope_processing() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let (charlie, _c) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (commit_bytes, _welcome) = alice.add_member(&channel_id, &charlie_kps[0]).unwrap();

        let envelope = ChannelEnvelope::Commit(commit_bytes);
        let envelope_bytes = fauna_cbor::encode_canonical(&envelope).unwrap();

        let epoch_before = bob.current_epoch(&channel_id).unwrap();
        let env: ChannelEnvelope = fauna_cbor::decode_strict(&envelope_bytes).unwrap();
        let ChannelEnvelope::Commit(cb) = env else {
            panic!("expected a commit envelope");
        };
        bob.process_commit(&channel_id, &cb).unwrap();
        assert_eq!(bob.current_epoch(&channel_id).unwrap(), epoch_before + 1);
    }

    #[test]
    fn blob_key_cached_on_commit() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let (charlie, _c) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        // Get epoch 1 blob key before advancing
        let key_before = bob.export_blob_key(&channel_id).unwrap();

        // Alice adds Charlie — advances epoch
        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (commit, _welcome) = alice.add_member(&channel_id, &charlie_kps[0]).unwrap();

        // Bob processes commit — should cache epoch 1 key
        bob.process_commit(&channel_id, &commit).unwrap();

        // Old epoch key should be cached in storage
        let cached = bob.storage.get_blob_epoch_key(&channel_id.0, 1).unwrap();
        assert_eq!(cached, Some(key_before));

        // Current (new epoch) key should be different
        let key_after = bob.export_blob_key(&channel_id).unwrap();
        assert_ne!(key_before, key_after);
    }

    /// An end-to-end room's post base (`ui/feed.md` § Encryption at rest →
    /// *Room-restricted*): every member at one epoch derives the same secret,
    /// and it is its own exporter label — never the blob, chunk or
    /// subscription secret of the same epoch, so a room's posts share a key
    /// with nothing else the group seals.
    #[test]
    fn a_room_post_secret_is_one_key_per_epoch_and_shares_it_with_nothing() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        let (epoch, secret) = alice.export_room_post_secret(&channel_id).unwrap();
        assert_eq!(epoch, alice.current_epoch(&channel_id).unwrap());
        assert_eq!(bob.room_post_secret_at(&channel_id, epoch).unwrap(), secret);
        assert_ne!(secret, alice.export_blob_key(&channel_id).unwrap());
        assert_ne!(secret, alice.export_chunk_key(&channel_id).unwrap());
        assert_ne!(
            secret,
            alice.export_subscription_secret(&channel_id).unwrap().1
        );
    }

    /// History falls out of the scheme (ruling 6): a member keeps what it held
    /// when the group advances — whether the advance is its own commit or a
    /// peer's — and a member who joins after an epoch never held it.
    #[test]
    fn a_member_keeps_the_room_post_secret_of_an_epoch_its_group_left() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let (charlie, _c) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();
        let (epoch, secret) = alice.export_room_post_secret(&channel_id).unwrap();

        // Alice's OWN commit advances her group.
        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (commit, welcome) = alice.add_member(&channel_id, &charlie_kps[0]).unwrap();
        assert_eq!(alice.current_epoch(&channel_id).unwrap(), epoch + 1);
        assert_eq!(
            alice.room_post_secret_at(&channel_id, epoch).unwrap(),
            secret,
            "the committer keeps the epoch it left"
        );

        // Bob advances by processing Alice's commit.
        bob.process_commit(&channel_id, &commit).unwrap();
        assert_eq!(
            bob.room_post_secret_at(&channel_id, epoch).unwrap(),
            secret,
            "a peer keeps the epoch it left"
        );

        // Charlie joined after it: no secret for an epoch it never saw.
        let _ = charlie.join_from_welcome(welcome).unwrap();
        assert!(charlie.room_post_secret_at(&channel_id, epoch).is_err());
        assert_eq!(
            charlie.room_post_secret_at(&channel_id, epoch + 1).unwrap(),
            alice.export_room_post_secret(&channel_id).unwrap().1,
            "the epoch it joined at is every member's"
        );
    }

    #[test]
    fn conversation_blob_seals_and_peer_opens_same_epoch() {
        // Two members at the same epoch: alice seals an attachment blob, bob
        // opens it under the same channel's epoch key.
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        let plaintext = b"\x89PNG\r\n\x1a\nattachment-bytes-distinct";
        let sealed = alice
            .seal_conversation_blob(&channel_id, plaintext)
            .unwrap();

        // Sealed bytes are an AEAD envelope (nonce+ct), not the plaintext, and the
        // content-address is the BLAKE3 of the sealed bytes.
        assert_ne!(sealed.sealed.as_slice(), plaintext.as_slice());
        assert!(sealed.sealed.len() >= plaintext.len() + 12 + 16);
        assert_eq!(sealed.sealed_cid, *blake3::hash(&sealed.sealed).as_bytes());

        let opened = bob
            .open_conversation_blob(&channel_id, sealed.epoch, &sealed.sealed)
            .unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn conversation_blob_opens_at_prior_epoch_after_advance() {
        // Grace-decrypt: a blob sealed at epoch N still opens after the group
        // advances to N+1, via the per-epoch blob-key cache `process_commit`
        // populates.
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let (charlie, _c) = make_engine();
        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        let plaintext = b"sealed-before-the-membership-change";
        let sealed = alice
            .seal_conversation_blob(&channel_id, plaintext)
            .unwrap();
        let sealed_epoch = sealed.epoch;

        // Advance the epoch: alice adds charlie, bob processes the commit (which
        // caches the pre-advance blob key).
        let charlie_kps = charlie.generate_key_packages(1).unwrap();
        let (commit, _welcome) = alice.add_member(&channel_id, &charlie_kps[0]).unwrap();
        bob.process_commit(&channel_id, &commit).unwrap();
        assert_ne!(bob.current_epoch(&channel_id).unwrap(), sealed_epoch);

        // Bob still opens the blob sealed at the prior epoch.
        let opened = bob
            .open_conversation_blob(&channel_id, sealed_epoch, &sealed.sealed)
            .unwrap();
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn groups_persist_across_restart() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();
        let alice_secret = ActorKeypair::generate().signing_key().to_bytes();
        let bob_secret = ActorKeypair::generate().signing_key().to_bytes();

        let channel_id = {
            let alice = MlsEngine::new(ActorKeypair::from_secret(alice_secret), &db_path).unwrap();
            let bob_tmp = NamedTempFile::new().unwrap();
            let bob =
                MlsEngine::new(ActorKeypair::from_secret(bob_secret), bob_tmp.path()).unwrap();
            let bob_kps = bob.generate_key_packages(1).unwrap();
            let (channel_id, _welcome) = alice.create_group(&bob_kps).unwrap();
            assert!(alice.has_group(&channel_id));
            // save_state is called automatically by persist_group in create_group
            channel_id
        };

        // Re-create Alice from the same secret key and DB path
        let alice2 = MlsEngine::new(ActorKeypair::from_secret(alice_secret), &db_path).unwrap();
        assert_eq!(alice2.list_groups().len(), 1);
        assert!(alice2.has_group(&channel_id));
    }

    #[test]
    fn group_registry() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();

        // No groups initially
        assert!(alice.list_groups().is_empty());
        assert!(!alice.has_group(&ChannelId([0u8; 32])));

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, _welcome) = alice.create_group(&bob_kps).unwrap();

        let groups = alice.list_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], channel_id);
        assert!(alice.has_group(&channel_id));
    }

    #[test]
    fn encrypt_with_nonexistent_channel_errors() {
        let (engine, _tmp) = make_engine();
        let fake_channel = ChannelId([0xAA; 32]);
        let msg = ChannelMessage {
            sender: engine.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("should fail".into()),
            timestamp: Timestamp::now(),
        };
        let result = engine.encrypt(&fake_channel, &msg);
        assert!(
            result.is_err(),
            "encrypting to nonexistent channel must fail"
        );
    }

    #[test]
    fn decrypt_corrupted_ciphertext_errors() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
        let _ = bob.join_from_welcome(welcome).unwrap();

        let msg = ChannelMessage {
            sender: alice.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("tamper me".into()),
            timestamp: Timestamp::now(),
        };
        let mut ciphertext = alice.encrypt(&channel_id, &msg).unwrap();

        if let Some(byte) = ciphertext.last_mut() {
            *byte ^= 0xFF;
        }

        // OpenMLS may panic on corrupt ciphertext rather than returning Err.
        // Either outcome (Err result or panic) demonstrates that decryption
        // of corrupted bytes does not silently succeed.
        let outcome = std::panic::catch_unwind(|| bob.decrypt(&channel_id, &ciphertext));
        match outcome {
            Ok(result) => assert!(result.is_err(), "corrupted ciphertext must fail decryption"),
            Err(_panic) => { /* OpenMLS panicked — decryption correctly rejected the tampered ciphertext */
            }
        }
    }

    #[test]
    fn welcome_from_unrelated_group_errors() {
        let (alice, _a) = make_engine();
        let (bob, _b) = make_engine();
        let (charlie, _c) = make_engine();

        let bob_kps = bob.generate_key_packages(1).unwrap();
        let (_channel_id, welcome_for_bob) = alice.create_group(&bob_kps).unwrap();

        let welcome_bytes = welcome_for_bob.tls_serialize_detached().unwrap();
        let result = charlie.join_from_welcome_bytes(&welcome_bytes);
        assert!(result.is_err(), "welcome meant for another party must fail");
    }

    /// **A retired engine must not seal.** The probe this row was found by,
    /// promoted to a pin.
    ///
    /// `SqliteStorage::retire` poisons the *database* handle, and completely —
    /// but the hazard its own doc names is "two engines advancing one group's
    /// epochs fork the ratchet", and epochs do not live in SQLite. They live in
    /// the in-memory openMLS provider, which `encrypt` mutates without touching
    /// storage at all. So before the quiesce a retired engine returned **281
    /// bytes of valid application ciphertext** under the account's own leaf,
    /// advancing a sender ratchet nothing could persist while its successor
    /// sealed from the snapshot taken at the retire point — overlapping
    /// `(ratchet secret, generation)` pairs on one leaf, which is the fork the
    /// role's exclusivity exists to prevent
    /// (`account-data-plane.md` § Multi-instance concurrency).
    #[test]
    fn a_retired_engine_refuses_to_seal_decrypt_or_commit() {
        let (engine, _tmp) = make_engine();
        let channel = engine.create_solo_group().unwrap();
        let message = ChannelMessage {
            sender: engine.identity_actor_id(),
            sequence: 1,
            channel_epoch: 0,
            body: ChannelMessageBody::Text("before the hand-over".into()),
            timestamp: Timestamp::now(),
        };
        let sealed = engine
            .encrypt(&channel, &message)
            .expect("a live engine seals");

        engine.retire();

        assert!(
            matches!(engine.encrypt(&channel, &message), Err(MlsError::Retired)),
            "a retired engine sealed — its ciphertext is on one leaf with the \
             successor's, from overlapping ratchet generations"
        );
        assert!(
            matches!(engine.decrypt(&channel, &sealed), Err(MlsError::Retired)),
            "a retired engine advanced a receiver ratchet nothing will persist"
        );
        assert!(
            matches!(engine.self_update(&channel), Err(MlsError::Retired)),
            "a retired engine originated a Commit"
        );
        assert!(
            matches!(engine.generate_key_packages(1), Err(MlsError::Retired)),
            "a retired engine minted key packages the successor's store will not hold"
        );
        assert!(engine.is_retired());
    }

    /// **The hand-over leaves the group intact for the successor**: retire
    /// flushes, frees the role, and the next engine over the same database
    /// finds what the predecessor had.
    ///
    /// ⚠ This does NOT witness the flush/quiesce ORDERING, and saying so
    /// matters more than the assertion: measured, moving the quiesce ahead of
    /// the flush leaves every pin here green. The reason is that `save_state`
    /// is deliberately not among the guarded methods — a retired engine's save
    /// is already refused by the storage poison, so guarding it in the engine
    /// would add nothing, and with no guarded method between the two the order
    /// is unobservable. The flag is still set last, defensively, so that the
    /// ordering stays correct if a future guard ever does sit there. A pin that
    /// claimed to prove the ordering would be claiming more than it can.
    #[test]
    fn retire_hands_the_group_over_intact() {
        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_path_buf();
        let secret = ActorKeypair::generate().signing_key().to_bytes();

        let channel = {
            let predecessor = MlsEngine::new(ActorKeypair::from_secret(secret), &db_path).unwrap();
            let channel = predecessor.create_solo_group().unwrap();
            predecessor.retire();
            channel
        };

        let successor = MlsEngine::new(ActorKeypair::from_secret(secret), &db_path)
            .expect("retire frees the role for the successor");
        assert!(
            successor.has_group(&channel),
            "the retire's final flush must land BEFORE the quiesce — otherwise the \
             hand-over drops whatever the last mutation left unsaved"
        );
    }

    /// Read-only accessors stay open on a retired engine, deliberately: the
    /// hand-over is graceful precisely so teardown can still ask what the
    /// engine knew. Only the paths that mutate group state or put bytes on the
    /// wire refuse.
    #[test]
    fn a_retired_engine_still_answers_read_only_questions() {
        let (engine, _tmp) = make_engine();
        let channel = engine.create_solo_group().unwrap();

        engine.retire();

        assert!(engine.has_group(&channel));
        assert_eq!(engine.list_groups(), vec![channel]);
        assert_eq!(
            engine.own_leaf_identity(&channel),
            Some(engine.identity_actor_id())
        );
    }

    /// **The quiesce census, ENUMERATED rather than re-audited by hand.**
    ///
    /// `account-data-plane.md` § Multi-instance concurrency states the contract
    /// as a universal — `is_retired` gates *every* method that mutates group
    /// state — but the implementation is per-door, and a universal enforced by
    /// a hand-kept list is only ever as good as the last audit. That list has
    /// now been found short twice: `join_from_welcome` +
    /// `join_from_welcome_bytes` (the door the durable inbox actually takes,
    /// where an unguarded success ACKS the Welcome and loses it for the
    /// successor), and `forget_group` + `restore_from_provider_storage`.
    ///
    /// So the list keeps itself: this test re-derives the census from the
    /// source on every run and fails when a **new** group-state-mutating `&self`
    /// method appears without `ensure_live()`. It reads `engine.rs` through
    /// `include_str!`, so it cannot drift from the file it audits.
    ///
    /// The markers are deliberately narrow — the group map (`insert`/`remove`/
    /// `get_mut`), the openMLS constructors that persist into the provider
    /// (`MlsGroup::new*`, `StagedWelcome::new_from_welcome`, `into_group`), and
    /// the raw KV swap — so this asserts a real superset of the doors that can
    /// fork a ratchet, not every method that happens to touch the provider.
    /// Fauna's own side-band memos (channel-kind labels, welcome-sender
    /// records, pending-commit hashes) are out of the class on purpose: no
    /// other member ever reads them and they cannot fork an epoch.
    #[test]
    fn every_group_state_mutating_door_is_quiesce_guarded() {
        const SRC: &str = include_str!("engine.rs");

        /// Doors deliberately outside the quiesce class. Add here ONLY with a
        /// reason that survives the next reader; an empty list is the healthy
        /// state. (`export_provider_storage` never appears — it is a pure read,
        /// and the goal doc names it a declared non-goal.)
        const ALLOWLIST: &[(&str, &str)] = &[];

        let production = SRC.split("\nmod tests").next().expect("engine.rs body");
        let lines: Vec<&str> = production.lines().collect();

        // Every method defined directly in an `impl` block (4-space indent),
        // of ANY receiver kind, paired with the line its body starts on and
        // whether it takes `&self`. A body's extent is bounded by the NEXT
        // head regardless of kind — a non-`&self` fn sitting between two
        // `&self` doors must not fold its text into the earlier door's body:
        // `identity_actor_id` was absorbing `new_in_memory_forged_for_test`'s
        // body this way, and `clear_pending_commit` was absorbing
        // `build_credential_and_signer`'s.
        // Only `&self` heads are ever AUDITED below — the class this census
        // exists to police — but every head still narrows every OTHER head's
        // boundary.
        let mut heads: Vec<(usize, usize, String, bool)> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(rest) = line.strip_prefix("    ") else {
                continue;
            };
            if rest.starts_with(' ') {
                continue; // deeper than a method
            }
            let rest = rest
                .strip_prefix("pub(crate) ")
                .or_else(|| rest.strip_prefix("pub "))
                .unwrap_or(rest);
            let rest = rest.strip_prefix("async ").unwrap_or(rest);
            let Some(after) = rest.strip_prefix("fn ") else {
                continue;
            };
            let name: String = after
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            // Accumulate the signature until the body opens.
            let mut sig = String::new();
            let mut j = i;
            while j < lines.len() && j < i + 14 {
                sig.push_str(lines[j]);
                sig.push(' ');
                if lines[j].contains('{') {
                    break;
                }
                j += 1;
            }
            let is_self = sig.contains("&self");
            heads.push((i, j, name, is_self));
        }

        const MARKERS: &[&str] = &[
            "groups.insert(",
            "groups.remove(",
            "groups.get_mut(",
            "MlsGroup::new",
            "StagedWelcome::new_from_welcome(",
            ".into_group(",
            "*store = values;",
        ];

        let mut unguarded: Vec<String> = Vec::new();
        let mut audited = 0usize;
        let mut exempted = 0usize;
        for (k, (head, body_start, name, is_self)) in heads.iter().enumerate() {
            // The boundary is the NEXT head of any kind (bounds correctly even
            // when a non-`&self` fn sits between two doors); only `&self`
            // heads are ever audited.
            let end = heads.get(k + 1).map_or(lines.len(), |n| n.0);
            if !is_self {
                continue;
            }
            let body_lines = &lines[*body_start..end];
            let body = body_lines.join("\n");
            // `groups\n    .insert(` — the rustfmt-wrapped form — must count too.
            let flat = body.replace('\n', " ");
            let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
            let flat = flat.replace("groups .", "groups.");
            if !MARKERS.iter().any(|m| flat.contains(m)) {
                continue;
            }
            // Test-only attacker-simulation seams are not production doors —
            // never reached the audited class at all, unlike an ALLOWLIST
            // exemption below.
            let cfg_test = lines[head.saturating_sub(5)..*head]
                .iter()
                .any(|l| l.contains("#[cfg(") && l.contains("test"));
            if cfg_test {
                continue;
            }
            // The census REACHED this door either way — an ALLOWLIST
            // exemption is a written-down decision not to require the guard,
            // not a door the census never saw. Counting it here before the
            // exemption `continue` is what keeps the drift floor honest: it
            // measures the census's REACH over the source, never "how many
            // doors still need the guard". The old shape `continue`d before
            // this increment, so every written-down exemption cost the
            // floor's headroom — the first sanctioned use of `ALLOWLIST` left
            // zero margin, and the second read as marker drift rather than an
            // exemption.
            audited += 1;
            if ALLOWLIST.iter().any(|(n, _)| n == name) {
                exempted += 1;
                continue;
            }
            // A STATEMENT, never a comment naming the guard: every real call
            // site is `self.ensure_live()?;` on its own line —
            // `body.contains("ensure_live()")` let a comment like `//
            // Deliberately NOT calling self.ensure_live() here` satisfy the
            // census with no guard actually present.
            let guarded = body_lines
                .iter()
                .any(|l| l.trim_start().starts_with("self.ensure_live()"));
            if !guarded {
                unguarded.push(format!("{name} (line {})", head + 1));
            }
        }

        // The census's REACH over the source — genuinely independent of
        // ALLOWLIST now: an exemption still increments `audited` above, so
        // this can only fall on real marker/boundary drift, never on the
        // sanctioned escape path. Measured 2026-08-30: 16 audited, 0 exempted
        // (ALLOWLIST is empty), floor 15 — one door of headroom against
        // genuine drift, independent of however many exemptions accrue.
        assert!(
            audited >= 15,
            "the census matched only {audited} doors ({exempted} exempted via \
             ALLOWLIST) — the markers or the body-boundary scan have drifted from \
             the source and this test is no longer auditing anything"
        );
        assert!(
            unguarded.is_empty(),
            "these methods mutate group state but never call `ensure_live()`, so a \
             RETIRED engine still runs them — `account-data-plane.md` § Multi-instance \
             concurrency requires the refusal, and on the Welcome path an unguarded \
             success also ACKS the durable inbox row and loses it: {unguarded:?}"
        );
    }

    /// **A retired engine refuses the WELCOME doors too** — the doors the
    /// per-door quiesce list missed (`account-data-plane.md` § Multi-instance
    /// concurrency).
    ///
    /// These are not a re-run of the seal/decrypt/commit pin: a Welcome join
    /// mutates only the **in-memory** provider (`StagedWelcome` writes, then
    /// the group lands in `self.groups`), so the retire's storage poison never
    /// reaches it. `persist_group` swallows both of its failures as `warn!`, so
    /// before the guard the join **succeeded** on a retired engine, held the
    /// group live, and could not persist a byte of it.
    ///
    /// Both doors are asserted because `join_from_welcome_bytes` is a duplicate
    /// join body rather than a delegate to `join_from_welcome` — guarding only
    /// the latter would leave the door the durable inbox drain actually takes
    /// wide open, which is exactly the miss this pin exists to prevent
    /// recurring. `forget_group` rides along: same class, same contract, and
    /// unguarded it half-applied (the in-memory removal landed before the
    /// poisoned store refused the durable one).
    #[test]
    fn a_retired_engine_refuses_every_welcome_door() {
        let (inviter, _tmp_a) = make_engine();
        let (joiner, _tmp_b) = make_engine();

        // A real Welcome, minted while both engines are live.
        let kps = joiner
            .generate_key_packages(2)
            .expect("a live engine mints key packages");
        let (channel, welcome) = inviter
            .create_group(&kps[0..1])
            .expect("a live engine creates the group");

        joiner.retire();

        assert!(
            matches!(joiner.join_from_welcome(welcome), Err(MlsError::Retired)),
            "a retired engine joined from a Welcome — it now holds a group whose \
             crypto state it can never persist"
        );
        assert!(
            !joiner.has_group(&channel),
            "the refusal must leave NO group behind: a half-joined group is the \
             ghost the quiesce exists to prevent"
        );

        // The second door, guarded in its own right (duplicate body, not a
        // delegate) — and the one the durable inbox drain actually takes.
        let (inviter2, _tmp_c) = make_engine();
        let (joiner2, _tmp_d) = make_engine();
        let kps2 = joiner2.generate_key_packages(1).expect("mint");
        let (_channel2, welcome2) = inviter2.create_group(&kps2).expect("create");
        let welcome_bytes = welcome2.to_bytes().expect("serialize the Welcome");
        joiner2.retire();
        assert!(
            matches!(
                joiner2.join_from_welcome_bytes(&welcome_bytes),
                Err(MlsError::Retired)
            ),
            "a retired engine joined through `join_from_welcome_bytes` — the door \
             the inbox drain takes, where an unguarded success ALSO acks the \
             durable row and loses the Welcome for the successor"
        );

        // Same class: destroying group state is as much a mutation as making it.
        let (owner, _tmp_e) = make_engine();
        let solo = owner.create_solo_group().expect("a live engine creates");
        owner.retire();
        assert!(
            matches!(owner.forget_group(&solo), Err(MlsError::Retired)),
            "a retired engine forgot a group — unguarded this half-applied, dropping \
             the in-memory group before the poisoned store refused the durable row"
        );
    }
}
