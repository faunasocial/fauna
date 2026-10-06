//! Native-client FFI for the **owner-side shared-folder content-key
//! orchestration** — the high-level `share_set` / `remove_member` /
//! `resume_pending_removals` calls that wrap the M2 MLS-group + content-key dance
//! so all 7 apps dispatch **one** call instead of re-implementing the
//! share / rotate-on-removal sequence per client (priority #2;
//! `docs/goal/architecture/mls-group-key-material.md` § M2 content-key mechanism,
//! `docs/goal/ui/folders.md` § Sharing a folder — *Where logic lives*: "Share
//! / remove orchestration — ✅ shared Rust: `FoldersAuthor::share_set` … +
//! `remove_member`"). The Rust-native Linux app calls
//! `fauna_client_folders::orchestration::FoldersAuthor` directly; this seam gives
//! Apple / Windows / Android the identical high-level surface over UniFFI, and
//! `fauna-wasm` gives the web SPA its twin.
//!
//! The thin per-kind reads + plaintext writes live on the companion
//! [`crate::FfiFoldersClient`] (`folders_client.rs`); this module is the
//! **encrypted-mode share orchestration** — the owner mints the per-set content
//! key + drives the MLS group, the nest holds only opaque bytes
//! (`mls-group-key-material.md` § M2). The direct analog of
//! [`crate::subscriptions_author`], differing only in the distribution channel: a
//! shared set distributes its content key over an **MLS group** (sealed under the
//! group epoch) instead of per-subscriber `KeyBlob` wraps.
//!
//! ## Engine reuse — ONE `MlsEngine` per `mls_state.db`
//!
//! The MLS-group operations need the actor's live [`MlsEngine`]. Per
//! `apps/fauna-linux/src/mls.rs` ("one engine over one mls_state.db, never two
//! racing on the same SQLite file") there is **exactly one** per-actor engine, the
//! one [`FfiNestClient::conversations_session`] built for the conversations rail.
//! So these fns take the live [`ConversationsSession`] and reuse its engine
//! ([`ConversationsSession::engine`]) as the [`FolderGroupCrypto`] adapter
//! (`impl … for Arc<MlsEngine>`, `fauna-client-folders`'s `mls` feature) — a
//! folder's MLS group thus lives on the same engine + db as the chat groups
//! (welcome/commit reuse) and survives a crash. Per priority #2 there is **no**
//! new logic here: each fn rebuilds the [`FoldersAuthor`] (the thin folders
//! client + the owner's content-key custody + the reused engine)
//! and dispatches one orchestration call.
//!
//! Gated behind the default-on `folders-author` feature (which flips on
//! `fauna-client-folders/mls`), exactly like `subscriptions-author`: the Go
//! mail-bridge `--no-default-features` build drops it (the bridge is a server with
//! no share UI, and the fns take a cross-crate `ConversationsSession` Object +
//! call the gated `FfiNestClient::nest_arc()`, which a featureless build would not
//! compile — same Go-incompatibility reason as `conversations-session`).
//!
//! Also home to [`wire_devices_mls_query`]: not owner-side orchestration, but
//! placed here (rather than `src/devices.rs`) because it needs the exact same two
//! dependencies this module already carries — a live [`ConversationsSession`] (for
//! its reused per-actor [`MlsEngine`]) and the `folders-author`-gated compile unit
//! — instead of making `src/devices.rs` (behind the plain `folders` feature) pull
//! both in for one function.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_conversations::ConversationsClient;
use fauna_client_folders::orchestration::FoldersAuthor;
use fauna_conversations::ConversationsSession;
use fauna_devices_machine::{DevicesMachine, MlsQuery};
use fauna_mls::engine::MlsEngine;

use crate::nest_client::FfiNestClient;
use crate::{FfiError, bytes_to_actor_id, general_err, keypair_from_bytes};

/// The owner-side author, generic over the native transport (`Arc<NestClient>`)
/// and the real MLS-engine [`FolderGroupCrypto`] adapter (`Arc<MlsEngine>`).
type NativeAuthor = FoldersAuthor<Arc<NestClient>, Arc<MlsEngine>>;

/// The outcome of a successful [`folders_share`] — the new shared set's derived
/// `ChannelId` (32 bytes, the custody/roster address) + the nest inbox row id the
/// member's Welcome was delivered to.
#[derive(uniffi::Record)]
pub struct FfiShareOutcome {
    /// The shared set's derived `ChannelId` (32 bytes).
    pub channel_id: Vec<u8>,
    /// The nest inbox row id the member's Welcome landed in.
    pub inbox_id: i64,
}

/// The outcome of a [`folders_remove_member`] (or a resumed removal) — the
/// `commit`/`evicted`/`rotated` fields of `RemoveOutcome`.
#[derive(uniffi::Record)]
pub struct FfiRemoveOutcome {
    /// The MLS Remove commit bytes this removal produced — **already distributed
    /// to the remaining members by the orchestration** (5d(d): `drive_removal`
    /// posts them to the set's channel; the members' folder commit poll
    /// advances their epoch). Observability only — clients must NOT re-send;
    /// `None` when the member was already absent (a resumed/no-op removal
    /// produced no new commit), and always `None` on the gated route (the
    /// device-owned-epoch rebase loop distributes inside the loop).
    pub commit: Option<Vec<u8>>,
    /// Whether the nest roster row was actually removed (`false` on a resumed
    /// eviction — already gone).
    pub evicted: bool,
    /// Whether a rotation actually happened. `false` ⇒ the member was never in the
    /// group and nothing was staged (a pure no-op).
    pub rotated: bool,
}

/// Rebuild the actor's shared-folder author over this connection's WS-RPC
/// transport — the shared `fauna_client_folders::build_folders_author` recipe
/// (thin `fauna.folders.*` client + the seat's
/// folder-key custody + the conversations rail's **shared** per-actor
/// [`MlsEngine`] as the group-crypto seam, wired
/// with its `FolderCommitGate`). Cheap to build per call (the engine `Arc` is
/// cloned, not re-opened).
fn author(
    nest: &Arc<FfiNestClient>,
    session: &Arc<ConversationsSession>,
    owner_secret: &[u8],
) -> Result<NativeAuthor, FfiError> {
    let keypair = keypair_from_bytes(owner_secret)?;
    Ok(fauna_client_folders::build_folders_author(
        nest.nest_arc(),
        keypair,
        crate::account_runtime::folder_key_store(),
        crate::account_runtime::mail_store(),
        session,
    )
    .with_grant_log(crate::ledger_seam()))
}

/// Decode a 32-byte derived `ChannelId` from the raw FFI bytes.
fn bytes_to_channel_id(bytes: &[u8]) -> Result<[u8; 32], FfiError> {
    bytes.try_into().map_err(|_| FfiError::General {
        msg: "channel ID must be 32 bytes".into(),
    })
}

/// Share an owner-only folder with one member end-to-end
/// ([`FoldersAuthor::share_set`]): fetch the member's KeyPackage → create the MLS
/// group admitting them → `fauna.folders.share` (the nest first-binder-claims the
/// channel) → bind the genesis content key + publish the envelope (custody-first,
/// crash-safe) → deliver the Welcome (registers the member on the roster).
/// `member_nest_url` is `None` for a same-nest member (the common case) or the
/// peer nest's base URL for a cross-nest share. The member must have published a
/// KeyPackage first (else the error carries `NoKeyPackage`).
///
/// One call admits **one** member + creates the set's group; sharing with a
/// further member of an already-shared set is a separate add-to-existing flow
/// (MLS Add + envelope republish) not yet built (the orchestration exposes no
/// `add_member` — a tracked follow-on).
///
/// `access` is the invited member's grant — `Some("writer")` for a read-write
/// share, `None` (or `Some("reader")`) for the read-only default (multi-writer
/// Phase 1; recorded nest-side with the bind).
#[fauna_uniffi_async::export]
pub async fn folders_share(
    nest: Arc<FfiNestClient>,
    session: Arc<ConversationsSession>,
    owner_secret: Vec<u8>,
    name: String,
    member_id: Vec<u8>,
    member_nest_url: Option<String>,
    access: Option<String>,
) -> Result<FfiShareOutcome, FfiError> {
    let author = author(&nest, &session, &owner_secret)?;
    let member = bytes_to_actor_id(&member_id)?;
    let convs = ConversationsClient::new(nest.nest_arc());
    author
        .share_set(&convs, &name, member, member_nest_url, access)
        .await
        .map(|o| FfiShareOutcome {
            channel_id: o.channel_id.to_vec(),
            inbox_id: o.inbox_id,
        })
        .map_err(general_err)
}

/// Remove a member from a shared set ([`FoldersAuthor::remove_member`]): MLS
/// Remove (epoch advances) → rotate to a fresh content key → re-publish the
/// envelope under the new epoch → evict the member from the nest roster → commit
/// the rotation. Crash-staged before the publish; a no-op (member already absent,
/// nothing staged) returns `rotated = false` without rotating. `channel_id` is the
/// set's 32-byte derived `ChannelId` (from [`FfiShareOutcome::channel_id`] /
/// custody).
#[fauna_uniffi_async::export]
pub async fn folders_remove_member(
    nest: Arc<FfiNestClient>,
    session: Arc<ConversationsSession>,
    owner_secret: Vec<u8>,
    name: String,
    channel_id: Vec<u8>,
    member_id: Vec<u8>,
) -> Result<FfiRemoveOutcome, FfiError> {
    let author = author(&nest, &session, &owner_secret)?;
    let channel = bytes_to_channel_id(&channel_id)?;
    let member = bytes_to_actor_id(&member_id)?;
    author
        .remove_member(&name, channel, member)
        .await
        .map(|o| FfiRemoveOutcome {
            commit: o.commit,
            evicted: o.evicted,
            rotated: o.rotated,
        })
        .map_err(general_err)
}

/// Flip a set's WebDAV serve state end-to-end ([`FoldersAuthor::serve_set`]) —
/// the production caller of slice-2's serve orchestration the Settings → File
/// sets `folder-webdav-toggle` drives: `serve_enable`/`serve_disable`
/// (content-key genesis/rotation + the nest `webdav_enabled` flag) then
/// `reconcile_webdav_keys_blob` (the MSEK-sealed `WebdavKeysBlob` re-provision).
/// `mls_group_id_hex` is the set's raw MLS group id
/// (`FolderSummary::mls_group_id`) when the set is shared, `None` when
/// owner-only; the derived `ChannelId` is computed here. Returns the number of
/// served sets the re-provisioned blob now carries.
///
/// Gate the toggle on [`folders_can_serve_webdav`]: an actor with no mail
/// credential cannot serve, and this call flips the nest flag *before* it
/// re-provisions, so a doomed enable would commit the flag and then fail
/// `NoMsek` (webdav-server.md § Independent enablement).
///
/// `device_id` (hex) is the app's recording device — the one its Media
/// gestures record under. With it, an enable of an unshared set also re-seals
/// the set's pre-serve files onto the served key before answering (the
/// flipping client's walk, `webdav-server.md` § Key model (c)); without it
/// that walk does not run.
#[fauna_uniffi_async::export(default(device_id = None))]
pub async fn folders_serve_set(
    nest: Arc<FfiNestClient>,
    session: Arc<ConversationsSession>,
    owner_secret: Vec<u8>,
    name: String,
    mls_group_id_hex: Option<String>,
    enable: bool,
    device_id: Option<String>,
) -> Result<u32, FfiError> {
    let mut author = author(&nest, &session, &owner_secret)?;
    if let Some(device_id) = device_id {
        author = author.with_served_set_converge(fauna_client_folders::served_set_converge(
            nest.nest_arc(),
            &keypair_from_bytes(&owner_secret)?,
            crate::account_runtime::folder_key_store(),
            device_id,
            // The attested ids are not handed across this face yet: the native
            // hosts (android, apple, windows) pass none, so the walk proves a
            // retired identity's link by the statement walk until they do.
            Vec::new(),
        ));
    }
    let channel_id = match mls_group_id_hex {
        Some(h) => {
            let raw = hex::decode(&h).map_err(|e| FfiError::General {
                msg: format!("mls_group_id must be valid hex: {e}"),
            })?;
            Some(fauna_mls::types::ChannelId::from_group_id(&raw).0)
        }
        None => None,
    };
    author
        .serve_set(&name, channel_id, enable)
        .await
        .map(|count| count as u32)
        .map_err(general_err)
}

/// Paywall a `web`-mode folder to a subscription tier end-to-end
/// ([`FoldersAuthor::paywall_set`]) — the Pillar-2 web-paywall analogue of
/// [`folders_serve_set`], the production caller of slice-6's paywall
/// orchestration the per-set "paywall to tier" control drives: content-key
/// genesis → the nest `web_paywall_tier` flag →
/// the `content.read{folder:set}` grant minted to the nest's **web-serve
/// holder** (`monetization.md` § Pillar 2 folder half; `web-content-hosting.md`
/// § Sealed static files). Each crash point leaves the set's exposure ≤ intent
/// (sealed-but-flagless ⇒ 404; flagged-but-grantless ⇒ the teaser, never the
/// content).
///
/// `mls_group_id_hex` is the set's raw MLS group id (`FolderSummary::mls_group_id`)
/// when the set is shared, `None` when owner-only (the serve pseudo-channel is
/// derived by the orchestration); the derived `ChannelId` is computed here, exactly
/// as [`folders_serve_set`]. The grant's seal target — the web-serve holder's
/// X25519 (+ optional ML-KEM ek for the X-Wing hybrid wrap) — is discovered via
/// the shared [`FoldersAuthor::discover_web_serve_holder`]
/// (`fauna.bridges.fetch_bridge_pubkey` for `("content-processor", "web-serve")`,
/// the [`fauna_client_bridges::discover_holders`] holder-discovery → mint precedent).
/// A nest with no enrolled web-serve holder (paywalled serving stays dark)
/// surfaces the `fauna.bridges.not_found` error.
#[fauna_uniffi_async::export]
pub async fn folders_paywall_set(
    nest: Arc<FfiNestClient>,
    session: Arc<ConversationsSession>,
    owner_secret: Vec<u8>,
    name: String,
    tier: String,
    mls_group_id_hex: Option<String>,
) -> Result<(), FfiError> {
    let author = author(&nest, &session, &owner_secret)?;
    let channel_id = match mls_group_id_hex {
        Some(h) => {
            let raw = hex::decode(&h).map_err(|e| FfiError::General {
                msg: format!("mls_group_id must be valid hex: {e}"),
            })?;
            Some(fauna_mls::types::ChannelId::from_group_id(&raw).0)
        }
        None => None,
    };
    let (holder_pubkey, holder_mlkem_ek) = author
        .discover_web_serve_holder()
        .await
        .map_err(general_err)?;
    author
        .paywall_set(&name, &tier, channel_id, holder_pubkey, holder_mlkem_ek)
        .await
        .map_err(general_err)
}

/// Re-provision a paywalled set's grant after its content key rotated
/// ([`FoldersAuthor::rotate_paywall_grant`]) — the **rotation leg** of the
/// web-paywall lifecycle (`monetization.md` § Pillar 2 folder half;
/// `mls-group-key-material.md` § M2: *"Rotation appends the new generation's wrap
/// via `fauna.capabilities.renew`"*). Call it after a content-key rotation
/// advanced the set's generation so the web-serve holder gains the new
/// generation's wrap and an entitled visitor's fresh token opens the
/// newest-sealed bytes; without it those bytes darken to the teaser.
///
/// `mls_group_id_hex` / holder discovery mirror [`folders_paywall_set`] exactly:
/// the set's raw MLS group id when shared, `None` when owner-only; the web-serve
/// holder is discovered via the shared [`FoldersAuthor::discover_web_serve_holder`].
/// The grant id is re-derived inside the orchestration (deterministic from the
/// owner secret + set name), so no grant-id state crosses the mint→rotate boundary.
///
/// The **automatic** callers live in the orchestration itself (wired 2026-07-15,
/// closing the dark-rail-audit residual): rotate-on-removal re-provisions inline
/// (`FoldersAuthor::finish_rotation`'s hook) and every app launch runs the
/// keep-alive sweep (`FoldersAuthor::renew_paywalled_grants`, riding
/// `resume_pending_removals`). This face is the *manual* twin for a client-driven
/// re-provision.
#[fauna_uniffi_async::export]
pub async fn folders_rotate_paywall(
    nest: Arc<FfiNestClient>,
    session: Arc<ConversationsSession>,
    owner_secret: Vec<u8>,
    name: String,
    mls_group_id_hex: Option<String>,
) -> Result<(), FfiError> {
    let author = author(&nest, &session, &owner_secret)?;
    let channel_id = match mls_group_id_hex {
        Some(h) => {
            let raw = hex::decode(&h).map_err(|e| FfiError::General {
                msg: format!("mls_group_id must be valid hex: {e}"),
            })?;
            Some(fauna_mls::types::ChannelId::from_group_id(&raw).0)
        }
        None => None,
    };
    let (holder_pubkey, holder_mlkem_ek) = author
        .discover_web_serve_holder()
        .await
        .map_err(general_err)?;
    author
        .rotate_paywall_grant(&name, channel_id, holder_pubkey, holder_mlkem_ek)
        .await
        .map_err(general_err)
}

/// Un-paywall a `web`-mode folder ([`FoldersAuthor::unpaywall_set`]) — the
/// **revoke leg**, the inverse of [`folders_paywall_set`] the client's "clear
/// the paywall" control drives (`monetization.md` § Pillar 2: *"revoking a grant
/// darkens exactly that slice"*). Revokes the set's grant (the web-serve holder's
/// next fetch zeroizes the key ⇒ sealed bytes darken to the teaser) and clears the
/// nest tier flag (a sealed row with no tier fails closed to 404), in that
/// crash-safe order. Takes **no holder discovery and no `mls_group_id`** — revoke
/// is keyed on the derived `(owner, grant_id)` alone, and the flag clear is by set
/// name. The files stay sealed at rest; this darkens, it does not re-publish them.
#[fauna_uniffi_async::export]
pub async fn folders_unpaywall_set(
    nest: Arc<FfiNestClient>,
    session: Arc<ConversationsSession>,
    owner_secret: Vec<u8>,
    name: String,
) -> Result<(), FfiError> {
    let author = author(&nest, &session, &owner_secret)?;
    author.unpaywall_set(&name).await.map_err(general_err)
}

/// Whether the owner can serve **any** set over WebDAV — the capability that
/// gates [`folders_serve_set`], so clients render `folder-webdav-toggle`
/// disabled with a "set up mail first" hint instead of letting the actor click
/// into a `NoMsek` failure (webdav-server.md § Independent enablement point 2).
///
/// Takes **no `ConversationsSession`** — unlike the serve itself, the question
/// needs only the account's mail custody (its MSEK), so a folders page can ask
/// it at render time without the conversations rail being wired.
#[fauna_uniffi_async::export]
pub async fn folders_can_serve_webdav(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<bool, FfiError> {
    let _ = (nest, keypair_from_bytes(&owner_secret)?);
    fauna_client_folders::owner_can_serve_webdav(crate::account_runtime::mail_store().as_ref())
        .await
        .map_err(general_err)
}

/// Re-drive every staged member-removal whose publish was interrupted by a crash
/// ([`FoldersAuthor::resume_pending_removals`]) — so the rotate-on-removal
/// forward-secrecy guarantee actually completes; the irrecoverable rotated key is
/// persisted before the publish, so a crash between would otherwise leave the
/// rotation half-done. Returns the count resumed.
///
/// **You do NOT need to call this at startup — the launch drives it for you.**
/// `mls_sync_launch::resume_folder_removals` runs it on every native session at
/// the MLS-replica restore-completion point (the first moment the removal gate is
/// live), and the factory injects that launcher into every session, so apple /
/// android / windows are covered with no client glue. This export remains for a
/// *manual* re-drive — e.g. after a config sync that may have merged a peer
/// device's staging, or a future "retry stuck removal" affordance.
///
/// (Said plainly because the previous wording — "Call on client startup" — was an
/// instruction no client ever followed: this entry point sat exported, documented
/// and fully tested with **zero callers on all six apps**, so crash-staged
/// removals were never re-driven and the guarantee silently did not hold until
/// 2026-07-12. A doc that names *who calls it* is falsifiable; one that tells a
/// client to call it is not. See `mls-group-key-material.md` § M2
/// *Rotate-on-removal*.)
#[fauna_uniffi_async::export]
pub async fn folders_resume_pending_removals(
    nest: Arc<FfiNestClient>,
    session: Arc<ConversationsSession>,
    owner_secret: Vec<u8>,
) -> Result<u32, FfiError> {
    let n = author(&nest, &session, &owner_secret)?
        .resume_pending_removals()
        .await
        .map_err(general_err)?;
    Ok(n as u32)
}

/// Derive the 32-byte content-channel `ChannelId` from a folder's hex-encoded
/// MLS group id (`FolderSummary::mls_group_id`) — the exact input
/// [`folders_remove_member`]'s `channel_id` needs for an **already-shared** set
/// loaded from a fresh snapshot (where no in-hand [`FfiShareOutcome::channel_id`]
/// exists because the share happened in a prior session / on another device).
///
/// The derivation is `blake3::derive_key("fauna.channel.v1", group_id)`
/// ([`fauna_mls::types::ChannelId::from_group_id`]), so it **cannot** be
/// reproduced client-side without duplicating the KDF + domain-separation string.
/// This pure seam gives Apple / Windows / Android the same one-line derivation the
/// Rust-native Linux app does inline (`apps/fauna-linux/src/client.rs`,
/// `ChannelId::from_group_id`, the LEAD); web's twin lives in `fauna-wasm`.
/// Returns the 32 raw (derived) bytes; a malformed (non-hex) group id is a
/// `General` error.
///
/// ⚠ **The MLS group id is NOT itself 32 bytes** — `MlsGroup::new`
/// (`fauna-mls/src/engine.rs::create_group`) never sets an explicit
/// `.group_id(...)`, so OpenMLS mints its own random id, which is 16 bytes —
/// `ChannelId::from_group_id` (and every in-repo Rust caller, e.g.
/// `engine.rs`'s own `ChannelId::from_group_id(group.group_id().as_slice())`)
/// already treats it as an arbitrary-length slice fed to a BLAKE3 KDF, never a
/// fixed-size identifier. Decode with a plain hex decoder, not `hex32::decode`
/// (which rejects anything but exactly 32 bytes / 64 hex chars — the bug found
/// 2026-07-12 driving the apple folder-sharing recipient-removal e2e for the
/// first time: `hex32::decode` 404'd every real (16-byte) group id).
#[uniffi::export]
pub fn folder_channel_id_from_group_id(group_id_hex: String) -> Result<Vec<u8>, FfiError> {
    fauna_mls::types::ChannelId::from_group_id_hex(&group_id_hex)
        .map(|id| id.0.to_vec())
        .map_err(|e| FfiError::General {
            msg: format!("invalid MLS group id: {e}"),
        })
}

/// Adapts a live [`ConversationsSession`]'s reused per-actor [`MlsEngine`] to the
/// pure `fauna_devices_machine::MlsQuery` seam [`DevicesMachine`] needs for its B3
/// member-row **join-filter**: the nest returns *rostered* members for a shared
/// folder (it cannot itself observe a client-side MLS join), so a
/// `role == "member"` row must be shown **only if** this client has actually
/// joined the group — else a stranger's un-accepted knock would surface in the
/// folders list unbidden (`fauna_devices_machine::machine::MlsQuery` doc;
/// `docs/goal/ui/folders.md` § Sharing).
///
/// `DevicesMachine::set_mls_query` takes `Arc<dyn MlsQuery>`, which — like the
/// machine's other injected seams (`DevicesNestApi`, `WizardFactory`) — has no FFI
/// ABI, so no UniFFI app (Apple/Android/Windows) had any way to wire it before
/// this adapter + [`wire_devices_mls_query`] existed. The Rust-native Linux app
/// wires it directly (`DevicesMachine::set_mls_query` is a plain in-process call,
/// no FFI boundary to cross); this struct is the equivalent adapter for the three
/// UniFFI apps, built once here instead of duplicated three times (priority #2).
///
/// **It holds the session WEAKLY** (`transport-connection.md` § No dialer
/// outlives its owner → *A holder the session cannot see never pins a
/// dialer*). The machine is held by app glue whose lifetime this crate cannot
/// see, and a strong hold made every undisposed machine keep its session — and
/// so the session's receive loop and index-lease tasks — alive for the life of
/// the process: measured on windows, one leaked session per e2e reset, each
/// still retrying against its departed client and holding the account store's
/// files open past the sign-out's erase. A departed session answers the same
/// fail-safe `false` an unwired machine does.
struct SessionMlsQuery {
    session: std::sync::Weak<ConversationsSession>,
}

impl MlsQuery for SessionMlsQuery {
    fn is_joined_shared_set(&self, mls_group_id_hex: &str) -> bool {
        let Some(session) = self.session.upgrade() else {
            return false;
        };
        match channel_id_from_hex(mls_group_id_hex) {
            Some(channel_id) => session.engine().has_group(&channel_id),
            // Fail-safe: an undecodable group id can never have been joined —
            // never panic on a malformed `mls_group_id` from a folder row.
            None => false,
        }
    }
}

/// Decode a raw hex MLS group id into its derived [`fauna_mls::types::ChannelId`],
/// or `None` on malformed hex (never panics). The same decode-then-derive
/// [`folder_channel_id_from_group_id`] performs, extracted here so
/// [`SessionMlsQuery::is_joined_shared_set`]'s fail-safe branch is independently
/// unit-testable without needing a real `ConversationsSession`/`MlsEngine`.
///
/// ⚠ Plain hex, NOT `hex32::decode` — same reason as
/// [`folder_channel_id_from_group_id`]: the real MLS group id is 16 bytes, not
/// 32. Before this fix (found 2026-07-12) `hex32::decode` rejected every real
/// group id here too, so `is_joined_shared_set` silently fail-closed on EVERY
/// shared set — the B3 "shared with me" row could never have rendered for any
/// UniFFI app (apple/windows/android), even after a real accepted join.
fn channel_id_from_hex(mls_group_id_hex: &str) -> Option<fauna_mls::types::ChannelId> {
    fauna_mls::types::ChannelId::from_group_id_hex(mls_group_id_hex).ok()
}

/// Wire `devices`'s B3 member-row join-filter to `session`'s live MLS engine — the
/// UniFFI-reachable equivalent of the Rust-native Linux app's direct
/// `DevicesMachine::set_mls_query` call (see [`SessionMlsQuery`] for why no other
/// UniFFI app could do this before).
///
/// **Call once per `DevicesMachine` instance, before the first `refresh()` /
/// `Refresh()` call.** Not a correctness bug if called later — the *next* refresh
/// after wiring will show previously-filtered member rows — but the machine is
/// fail-safe (every `role == "member"` row dropped) until wired, so a first render
/// before this call shows a shared-with-me folder as absent, not merely stale.
/// `session` must be the client's one per-actor `ConversationsSession` (the same
/// engine over the same `mls_state.db`, never a second one racing on that file).
#[uniffi::export]
pub fn wire_devices_mls_query(devices: Arc<DevicesMachine>, session: Arc<ConversationsSession>) {
    devices.set_mls_query(Arc::new(SessionMlsQuery {
        session: Arc::downgrade(&session),
    }));
}

/// Wire `devices`'s foreign-set (cross-nest) list source — the UniFFI-reachable
/// equivalent of the Rust-native Linux app's direct
/// `DevicesMachine::set_foreign_sets_source` call (Phase 2 client read-side):
/// sets shared from ANOTHER nest have no row in the own nest's list, so the
/// machine unions in the member's own custody records (written at
/// share-accept; read via the shared `CustodyForeignSetsSource` over the seat's
/// folder-key custody, priority #2). `nest` and `owner_secret` keep the
/// exported signature: the secret still gates the call, and the custody is the
/// seat's.
/// Call once per `DevicesMachine`, beside [`wire_devices_mls_query`] — unwired,
/// the list simply carries no foreign rows.
#[uniffi::export]
pub fn wire_devices_foreign_sets(
    devices: Arc<DevicesMachine>,
    nest: Arc<crate::FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<(), FfiError> {
    keypair_from_bytes(&owner_secret)?;
    let _ = nest;
    devices.set_foreign_sets_source(Arc::new(
        fauna_devices_machine::CustodyForeignSetsSource::new(
            crate::account_runtime::folder_key_store(),
        ),
    ));
    Ok(())
}

/// Wire `devices`'s **followed public folders** source — the UniFFI-reachable
/// equivalent of the Rust-native Linux app's direct
/// `DevicesMachine::set_followed_folders_source` call
/// (`docs/goal/behavior/folders.md` § Publicly-synced follow).
///
/// A follow lives entirely in the user's own account store
/// (`fauna.state.follows`; the home nest keeps no follower state), so the
/// shared `StoreFollowedFoldersSource` reads it there and probes each folder's
/// availability. Call once per `DevicesMachine`,
/// beside [`wire_devices_foreign_sets`] — unwired, the page simply carries no
/// followed rows.
#[uniffi::export]
pub fn wire_devices_followed_folders(
    devices: Arc<DevicesMachine>,
    nest: Arc<crate::FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<(), FfiError> {
    devices.set_followed_folders_source(crate::build_followed_folders_source(&nest, owner_secret)?);
    Ok(())
}

/// Map the shared [`FollowOpError`] onto the FFI boundary's flat error, doing
/// the **two-arm match** once here rather than three times in Swift, Kotlin and
/// C#.
///
/// `NotFound` carries the single ratified wording from the shared catalog
/// (`follow_error_text`), which is the whole point of that arm being one arm:
/// absent, private and misspelled are folded by the home nest so nothing can
/// probe for the existence of a sealed folder, and an app that invented a
/// friendlier per-case message would hand back exactly the distinction the nest
/// refused to make. Anything else is a transport fault and says so.
#[cfg(feature = "folders-author")]
fn follow_op_err(e: fauna_client_folders::follow_ops::FollowOpError) -> FfiError {
    crate::general_err(fauna_client_folders::follow_ops::follow_error_text(e))
}

/// **Follow a public folder** — first contact, addressed by the OWNER (a handle
/// or a bare 64-hex actor id, the same superset `share_set` takes) plus the
/// folder's plaintext name, exactly as the follow flow collects them
/// (`docs/goal/ui/folders.md` § Following a public folder).
///
/// A thin façade over the shared composition
/// [`fauna_client_folders::follow_ops::follow_public_folder`], which classifies
/// the owner address, runs the first public fetch — what pins the home nest's
/// stable `folder_id`, so a later rename cannot break the follow — and persists
/// the record as the account's `fauna.state.follows` row for the folder
/// ([`crate::follows_seam`]). Returns the stored follow list so the page
/// renders exactly what was saved. `owner_secret` derives nothing any more; a
/// malformed one is still refused.
///
/// ⚠ **This deliberately takes `owner`, not a pre-resolved actor id.** The
/// address rules — hex-or-handle classification, the same-nest
/// `fauna.actor.by_handle` hop, the cross-nest discovery of a `handle@domain`'s
/// home nest (the anon hop to the peer, the record's `home_nest_url` derived
/// from the domain), the blank-field refusal, and the folding of the three
/// not-found causes — are the shared recipe's, and an app leg that resolved the
/// handle itself before calling would be the fourth re-derivation of them
/// (`follow_ops`'s own module docs name the three it replaced). The recipe
/// never asks for a nest url: the home nest is discovered from the handle's
/// domain, and the address a user types is a handle, not a nest.
#[fauna_uniffi_async::export]
pub async fn folders_follow_public(
    nest: Arc<crate::FfiNestClient>,
    owner_secret: Vec<u8>,
    owner: String,
    folder_name: String,
) -> Result<Vec<FfiFollowedFolder>, FfiError> {
    keypair_from_bytes(&owner_secret)?;
    let store = crate::follows_seam();
    fauna_client_folders::follow_ops::follow_public_folder(
        nest.nest_arc(),
        &*store,
        &owner,
        &folder_name,
    )
    .await
    .map_err(follow_op_err)?;

    // The recipe returns the ONE record it stored; the page wants the whole
    // list, so read it back through the same chokepoint `folders_followed_list`
    // uses rather than splicing the record into a locally-held list.
    let stored = fauna_client_config::load_followed_folders(&*store)
        .await
        .map_err(crate::general_err)?;
    Ok(stored.into_iter().map(FfiFollowedFolder::from).collect())
}

/// **Unfollow** — the account's row for the folder is tombstoned; there is
/// nothing to revoke anywhere, because the home nest never knew this follower
/// existed (the public read plane keeps zero follower state by design).
/// Idempotent: removing something already gone succeeds. Returns the stored
/// list. `nest` and `owner_secret` stay for the signature's sake: the secret is
/// still checked, the nest is not dialed.
///
/// Routes through the shared [`fauna_client_folders::follow_ops::
/// unfollow_public_folder`] for the same reason its sibling above does — one
/// composition, one error vocabulary, no per-app copy.
#[fauna_uniffi_async::export]
pub async fn folders_unfollow_public(
    nest: Arc<crate::FfiNestClient>,
    owner_secret: Vec<u8>,
    home_nest_url: String,
    folder_id: i64,
) -> Result<Vec<FfiFollowedFolder>, FfiError> {
    let _ = nest;
    keypair_from_bytes(&owner_secret)?;
    let store = crate::follows_seam();
    fauna_client_folders::follow_ops::unfollow_public_folder(&*store, &home_nest_url, folder_id)
        .await
        .map_err(follow_op_err)?;

    let stored = fauna_client_config::load_followed_folders(&*store)
        .await
        .map_err(crate::general_err)?;
    Ok(stored.into_iter().map(FfiFollowedFolder::from).collect())
}

/// The user's followed public folders as stored — the list without the
/// availability probe. The page's rows (which carry availability) come from
/// `DevicesSnapshot.followed`; this is for a caller that only needs the
/// records, e.g. to decide whether to offer "Follow" for an address already
/// followed.
#[fauna_uniffi_async::export]
pub async fn folders_followed_list(
    nest: Arc<crate::FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<Vec<FfiFollowedFolder>, FfiError> {
    let _ = nest;
    keypair_from_bytes(&owner_secret)?;
    let stored = fauna_client_config::load_followed_folders(&*crate::follows_seam())
        .await
        .map_err(crate::general_err)?;
    Ok(stored.into_iter().map(FfiFollowedFolder::from).collect())
}

/// A stored follow, over the FFI boundary.
///
/// Distinct from `fauna_devices_machine::FollowedFolderSummary`, which is the
/// *page row* and carries an availability verdict this list deliberately does
/// not: these three fns read and write the record, they do not probe the home
/// nest.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiFollowedFolder {
    /// The folder's home nest base URL; empty ⇒ the user's own nest.
    pub home_nest_url: String,
    /// Hex owner actor id.
    pub owner_actor_id: String,
    /// The home nest's stable `folders.id`, pinned at first contact.
    pub folder_id: i64,
    /// The folder's plaintext name as of the last successful read.
    pub display_name: String,
}

impl From<fauna_core::data::FollowedFolder> for FfiFollowedFolder {
    fn from(f: fauna_core::data::FollowedFolder) -> Self {
        Self {
            home_nest_url: f.home_nest_url,
            owner_actor_id: f.owner_actor_id,
            folder_id: f.folder_id,
            display_name: f.display_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_id_from_group_id_matches_direct_derivation() {
        // Any 32-byte hex group id resolves to the same ChannelId the native
        // apps would pass to `folders_remove_member`.
        let group = [0xabu8; 32];
        let group_hex = hex::encode(group);
        let got = folder_channel_id_from_group_id(group_hex.clone()).expect("valid hex32");
        let want = fauna_mls::types::ChannelId::from_group_id(&group)
            .0
            .to_vec();
        assert_eq!(got, want);
        assert_eq!(got.len(), 32);
        // Surrounding whitespace is trimmed (mirrors `ChannelId::from_hex`).
        let padded = format!("  {group_hex}\n");
        assert_eq!(folder_channel_id_from_group_id(padded).unwrap(), want);
    }

    #[test]
    fn channel_id_from_group_id_accepts_the_real_16_byte_mls_group_id() {
        // The MLS group id `MlsGroup::new` actually mints (`fauna-mls/src/
        // engine.rs::create_group`, no explicit `.group_id(...)`) is 16 bytes,
        // NOT 32 — this is the regression guard for the bug found 2026-07-12
        // (a `hex32::decode` 32-byte requirement here 404'd every real group id
        // driving the apple folder-sharing recipient-removal e2e for the
        // first time). `ChannelId::from_group_id` is a BLAKE3 KDF over an
        // arbitrary-length slice, so any length must decode successfully.
        let group16 = [0xefu8; 16];
        let group_hex = hex::encode(group16);
        let got = folder_channel_id_from_group_id(group_hex).expect("16-byte group id is valid");
        let want = fauna_mls::types::ChannelId::from_group_id(&group16)
            .0
            .to_vec();
        assert_eq!(got, want);
    }

    #[test]
    fn channel_id_from_group_id_rejects_malformed() {
        assert!(folder_channel_id_from_group_id("not-hex".into()).is_err());
        assert!(folder_channel_id_from_group_id("dea".into()).is_err()); // odd-length hex
    }

    #[test]
    fn channel_id_from_hex_matches_direct_derivation_and_fails_safe_on_malformed() {
        // `SessionMlsQuery::is_joined_shared_set`'s fail-safe branch (return
        // `false`, never panic, on an undecodable `mls_group_id`) hinges on this
        // helper returning `None` rather than erroring/panicking — verify both the
        // happy path (matches the direct derivation, at the REAL 16-byte group-id
        // length) and the fail-safe path here, since the adapter itself needs a
        // real `ConversationsSession` to test.
        let group = [0xcdu8; 16];
        let group_hex = hex::encode(group);
        let want = fauna_mls::types::ChannelId::from_group_id(&group);
        assert_eq!(channel_id_from_hex(&group_hex), Some(want));
        assert_eq!(channel_id_from_hex("not-hex"), None);
    }

    /// The seam holds its session weakly, so a `DevicesMachine` its host let go
    /// of without disposing never keeps a departed session — and with it that
    /// session's receive loop and index-lease tasks — alive
    /// (`transport-connection.md` § No dialer outlives its owner). A departed
    /// session answers the fail-safe `false`, never a panic.
    #[test]
    fn a_departed_session_answers_not_joined() {
        let query = SessionMlsQuery {
            session: std::sync::Weak::new(),
        };
        assert!(!query.is_joined_shared_set(&hex::encode([0x11u8; 16])));
    }
}
