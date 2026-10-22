//! The Python e2e harness's **networked seed exports** — the writes a fixture
//! makes straight over WS-RPC that since the writer-signed change records
//! (`mls-group-key-material.md` § M2 → *Multi-writer* → *Writer-signed change
//! records*, ruling (4)) must go out signed, and whose sets must be born
//! custody-first (the *Custody shape of the set nonce* sub-bullet, point (d)).
//!
//! Each export takes one decoded request and runs it through the SAME shared
//! funnel a client does — never a re-derivation of a signed statement (the
//! `fauna_ffi.py` rule "a fixture must never re-derive a signed envelope"):
//!
//! - [`fauna_harness_create_set`] — [`fauna_client_folders::create_set`]: the
//!   nonce minted into the owner's custody first, then `fauna.folders.create`
//!   carrying it. A set born any other way has no custody entry, so the owner
//!   app's launch reconcile mints a fresh nonce over the nest's copy and every
//!   row the harness signed under the old one stops verifying.
//! - [`fauna_harness_record_change`] — the nonce read fresh through
//!   [`fauna_client_folders::record_nonce`] (roster + custody), the request
//!   signed through [`fauna_client_sync::RecordSigning`] with the identity key
//!   ([`ChangeSigner::direct`]), then `fauna.sync.changes.record`.
//! - [`fauna_harness_report_conflict`] — the same for a pre-resolved
//!   `fauna.sync.conflicts.report` (its winner head row is the signed part).
//! - [`fauna_harness_sign_custody_witness`] — not networked: the owner's
//!   custody witness signed through
//!   [`fauna_core::custody_grant::sign_custody_grant`], for a test that
//!   deposits a hosting row without running the ceremony around it.
//! - [`fauna_harness_mint_custody`] / [`fauna_harness_revoke_custody`] — the
//!   owner's capability row for that custody, minted on the owner's nest in the
//!   ceremony's record → publish → deposit order (through a [`Seat`]'s ledger
//!   door), and revoked through the shared
//!   [`fauna_client_capabilities::custody_acts::revoke_custody`]: the halves a
//!   hosting nest's pump redeems the witness against.
//! - [`fauna_harness_custodied_fleet_handshake`] — an owner-fleet device's
//!   handshake at a CUSTODIAN nest through the shared
//!   [`fauna_anon_client::mint_bearer_over_custodied_fleet_handshake`], its
//!   root-signed cert minted here: the bearer a test reads a held copy back
//!   with, or the nest's refusal.
//! - [`fauna_harness_create_private_link`] —
//!   [`fauna_client_share::ShareClient::create_private_link`]: a fragment-keyed
//!   share link minted, registered, and its URL revealed against the reply.
//!   Its recipient-side twin, [`fauna_harness_share_viewer_open`], is not
//!   networked: the viewer's own open + assemble over bytes the test fetched.
//! - [`fauna_harness_mail_import`] — not a seed write but a whole client run:
//!   the shared `MailImportMachine` the apps build, walked from Source to Done
//!   against a harness IMAP source, so a `[nest]` witness of a claim the import
//!   CLIENT makes (`mailbox-migration.md` § Credential handling) observes a real
//!   import with no app in it. Its reply is the machine's final snapshot, not a
//!   nest reply. [`fauna_harness_mail_import_outlook`] is its Outlook twin,
//!   signing in with Microsoft against the harness's fake authority.
//! - [`fauna_harness_archive_import`] — the same for the shared
//!   `ArchiveImportMachine`: a fixture export archive walked from Source to
//!   Done, so a witness of what the import CLIENT leaves on the nest
//!   (`archive-import.md` § Architectural rules, § What each category becomes)
//!   observes a real import with no app in it.
//! - [`fauna_harness_dns_dispatch`] — the shared `DnsManagementMachine` the
//!   admin DNS page builds, its DNS record a [`Seat`]'s, actions dispatched in
//!   order: a managed `IssueCert` runs the whole client issue funnel against
//!   the e2e CA and fake provider, so a `[nest]` witness of what the issuing
//!   CLIENT orders (`tls-certificates.md` § B) reads the installed certificate
//!   with no app in it.
//! - [`fauna_harness_serve_set`] — the owner's per-set WebDAV toggle over an
//!   unshared set, through the shared
//!   `fauna_client_folders::orchestration::FoldersAuthor::serve_enable` /
//!   `serve_disable` (custody first, the nest's flag, and on serve-off the
//!   principals' folder read twins revoked), minus the MSEK-sealed
//!   `WebdavKeysBlob` re-provision an app-less seat cannot seal.
//! - [`fauna_harness_answer_consent`] — an owner's approve of a pending
//!   consent request, through the consent machine's shared `answer_consent`
//!   over the seams the apps wire: the consent-time grants planned, recorded,
//!   deposited, then the resolve.
//! - [`fauna_harness_topic_create`] / [`fauna_harness_topic_train`] /
//!   [`fauna_harness_cues_record_and_flush`] — the personalization planes an
//!   app writes and the nest only stores sealed: a trained topic minted through
//!   the shared `create_trained_topic` over a [`Seat`]'s store, a post marked
//!   through `FeedManager::train_post`, engagement verdicts folded and flushed
//!   through the manager's own cue path — so a `[nest]` witness of what the nest
//!   never learns (`topic-factors.md` § At rest, `engagement-cues.md` § At rest)
//!   reads the nest after a real write with no app in it.
//! - [`fauna_harness_peer_seat_hold`] — a [`Seat`] on a device row of its
//!   own with the peer leg bound, run pass after pass until released: two of
//!   them in two isolated boxes are two devices of one account reaching each
//!   other through the nest's relay (`p2p.md` § The relay), the leg's own
//!   pass report read back as each pass ends. The one difference from an
//!   app's leg is the relay root it trusts
//!   ([`fauna_iroh::peer_leg_transport_trusting`]).
//!
//! **How an app-less process reaches custody.** The account's folder-key
//! custody is the account plane's `fauna.state.folder-keys` kind and nothing
//! else (`config-dissolution.md`, the kinds table), so every export goes
//! through the custody seam (`fauna_client_folders::{FolderKeyReader,
//! FolderKeyStore}`), each half the way a seed holder with no app reaches it:
//!
//! - **A read** (the nonce a record signs under, the engine build of
//!   [`fauna_harness_sync_pass`]) is the seed holder's reader — a throwaway
//!   fleet replica keyed by the generations the seed recovers from escrow
//!   (`fauna_sync_engine::cold_folder_keys::seed_holder_folder_keys`). It
//!   authors nothing and leaves no trace on the account.
//! - **A write** ([`fauna_harness_create_set`]) needs a plane writer, and the
//!   plane admits only an enrolled device's rows — so the export hosts the
//!   account store for the length of the call, exactly as a seat does
//!   ([`Seat`]: `AccountStoreRuntime::start` as the seed-holding principal over
//!   a store and a credential slot under a temp dir that dies with the call).
//!   What that leaves on the account is what a real second device signing in
//!   leaves: one fleet enrollment per call, and one self-registered
//!   `sync_devices` row per account ([`seat_device_id`]). A test counting an
//!   account's devices creates its sets through an app, not through here.
//!
//! **Test surface, compiled out of release artifacts** (e2e convention 15,
//! owned by `e2e-automation-surface-gating.md`): the module exists only under
//! fauna-ffi's `e2e-harness` feature, which `just e2e-ffi` turns on and no
//! app build does. It is the only networked code in the C ABI.
//!
//! **Reply shape.** Every export writes one canonical dag-cbor map into `out`:
//! `{"ok": <the kind's reply>}` when the nest answered, `{"err": <RpcError>}`
//! when it refused (so a test asserts on the wire code exactly as it would
//! over its own WS client), and returns `0`. A non-zero return is a local
//! failure only — bad arguments, no connection, no nonce to sign under — with
//! the message in `fauna_ffi_last_error`.

use std::ffi::c_char;
use std::sync::{Arc, OnceLock};

use fauna_client::{NestClient, NestClientError};
use fauna_client_account_runtime::folder_keys::PlaneFolderKeys;
use fauna_client_folders::{FolderKeyReader, FoldersClient};
use fauna_client_sync::{RecordSigning, SetNonceSource};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::RpcRequester;
use fauna_protocol::folders::{
    ConflictReportReply, ConflictReportRequest, FolderCreateReply, FolderCreateRequest,
};
use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest};
use fauna_protocol::sync_writer_sig::ChangeSigner;
use fauna_sync_engine::account_runtime::{
    AccountRuntimeParams, AccountStoreHandle, AccountStoreRuntime, CRED_NAMESPACE,
    CloudBackupExclusion, DEFAULT_BACKSTOP_INTERVAL, PeerLegBinding, PeerLegFactoryInputs,
    PeerTransportFactory, RuntimePrincipal, StoreRoot, fixed_holders,
};

use super::{FfiBuffer, cstr_to_string, set_last_error, slice_to_vec};
use crate::FfiError;

fn err(msg: impl Into<String>) -> FfiError {
    FfiError::General { msg: msg.into() }
}

/// The one runtime every call blocks on. Multi-threaded because a connected
/// [`NestClient`] runs its reconnect supervisor as a spawned task that must
/// keep being polled while the caller's request waits on it.
fn runtime() -> Result<&'static tokio::runtime::Runtime, FfiError> {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    if let Some(rt) = RT.get() {
        return Ok(rt);
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| err(format!("tokio runtime: {e}")))?;
    Ok(RT.get_or_init(|| rt))
}

/// What the nest said, as the one-key dag-cbor map the export writes:
/// `{"ok": <reply>}` or `{"err": <RpcError>}` (each value already canonical
/// dag-cbor). Spliced by hand — a one-entry map is its header byte, the key's
/// text string, then the value — because this crate names no serde trait.
enum Outcome {
    Ok(Vec<u8>),
    Err(Vec<u8>),
}

impl Outcome {
    fn into_cbor(self) -> Vec<u8> {
        let (key, value): (&[u8], Vec<u8>) = match self {
            // 0x62 / 0x63 = a text string of 2 / 3 bytes.
            Outcome::Ok(v) => (b"\x62ok", v),
            Outcome::Err(v) => (b"\x63err", v),
        };
        let mut out = Vec::with_capacity(1 + key.len() + value.len());
        out.push(0xA1); // a map of one entry
        out.extend_from_slice(key);
        out.extend(value);
        out
    }
}

fn encode_err(e: impl std::fmt::Display) -> FfiError {
    err(format!("encode reply: {e}"))
}

/// A nest call's result as an [`Outcome`]: the reply, or the nest's refusal;
/// anything else (transport, auth) is a local failure. A macro so the reply's
/// concrete type reaches `canonical_encode` without this crate naming serde.
macro_rules! outcome {
    ($r:expr) => {
        match $r {
            Ok(v) => fauna_core::encoding::canonical_encode(&v)
                .map(Outcome::Ok)
                .map_err(encode_err),
            Err(NestClientError::Rpc(e)) => fauna_core::encoding::canonical_encode(&e)
                .map(Outcome::Err)
                .map_err(encode_err),
            Err(e) => Err(err(format!("nest call: {e}"))),
        }
    };
}

/// Decode a request as its concrete wire type (a macro for the same reason as
/// [`outcome!`]). Strict dag-cbor: the Python wrapper encodes with
/// `cbor2.dumps(..., canonical=True)`, whose length-first key order and minimal
/// integers are dag-cbor's.
macro_rules! decode_request {
    ($bytes:expr) => {
        fauna_core::encoding::canonical_decode(&$bytes)
            .map_err(|e| err(format!("decode request: {e}")))
    };
}

/// The arguments every export shares, the request still encoded.
struct Call {
    nest_url: String,
    identity: ActorKeypair,
    req: Vec<u8>,
}

/// # Safety
///
/// As the calling export's `# Safety` section.
unsafe fn decode_call(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    req: *const u8,
    req_len: u32,
) -> Result<Call, FfiError> {
    // SAFETY: forwarded from the export's contract.
    let (nest_url, secret, req) = unsafe {
        (
            cstr_to_string(nest_url)?,
            slice_to_vec(secret, secret_len),
            slice_to_vec(req, req_len),
        )
    };
    let seed: [u8; 32] = secret.as_slice().try_into().map_err(|_| {
        err(format!(
            "secret must be exactly 32 bytes (the Ed25519 seed), got {}",
            secret.len()
        ))
    })?;
    Ok(Call {
        nest_url,
        identity: ActorKeypair::from_secret(seed),
        req,
    })
}

/// Connect as `identity`, run `op`, disconnect — one connection per call, so
/// no supervisor outlives the test that dialled it (nests come and go on
/// reused ports across a run).
fn with_client<T, F, Fut>(nest_url: String, identity: ActorKeypair, op: F) -> Result<T, FfiError>
where
    F: FnOnce(Arc<NestClient>) -> Fut,
    Fut: std::future::Future<Output = Result<T, FfiError>>,
{
    let nest = NestClient::new(nest_url, identity);
    runtime()?.block_on(async move {
        nest.connect()
            .await
            .map_err(|e| err(format!("connect: {e}")))?;
        let result = op(Arc::clone(&nest)).await;
        nest.disconnect().await;
        result
    })
}

/// The account's folder-key custody as this app-less seed holder reads it
/// (module docs, *How an app-less process reaches custody*).
fn custody_reader(nest: &Arc<NestClient>, identity: &ActorKeypair) -> Arc<dyn FolderKeyReader> {
    fauna_sync_engine::cold_folder_keys::seed_holder_folder_keys(Arc::clone(nest), identity)
}

/// The `sync_devices` row every harness seat of one account enrolls on — one
/// id per account, so however many sets a run creates app-lessly the account
/// gains one self-registered device row, not one per call.
fn seat_device_id(identity: &ActorKeypair) -> String {
    hex::encode(blake3::derive_key(
        "fauna e2e harness seat device id v1",
        &identity.actor_id().0,
    ))
}

/// The account store, hosted for the length of one call (module docs, *How an
/// app-less process reaches custody*): the production runtime as the
/// seed-holding principal, over a store and a writer-key slot in a temp dir.
/// Everything a seat's first sign-in does happens here too — the writer key
/// minted, the device enrolled, the first generation minted at the first
/// custody write — which is what makes the rows it writes ones every other
/// replica of the account admits.
struct Seat {
    handle: AccountStoreHandle,
    /// The store and the slot; removed when the seat drops, after
    /// [`Self::stop`] closed the store.
    _dir: tempfile::TempDir,
}

impl Seat {
    /// Start the runtime over `nest` (connected as `identity`) and wait out its
    /// prologue pass, so the enrollment is published and whatever the account
    /// already holds is walked before the caller writes.
    async fn start(nest: &Arc<NestClient>, identity: &ActorKeypair) -> Result<Self, FfiError> {
        Self::start_on(nest, identity, seat_device_id(identity), None).await
    }

    /// [`Self::start`] enrolled on the device row `device_id`, with the peer
    /// leg's transport factory `peer_transport` — `None` keeps the leg off, as
    /// every seat but [`fauna_harness_peer_seat_hold`]'s runs.
    async fn start_on(
        nest: &Arc<NestClient>,
        identity: &ActorKeypair,
        device_id: String,
        peer_transport: Option<PeerTransportFactory>,
    ) -> Result<Self, FfiError> {
        let nest_url = nest.nest_url().to_string();
        // The escrow holder this seat trusts is the nest at the far end of its
        // own connection, proved by possession — the read an app's pin makes.
        let holder = fauna_client::trust::connection_bound_identity(nest, &nest_url)
            .await
            .map_err(|e| err(format!("read the identity {nest_url} is bound to: {e}")))?;
        let dir = tempfile::tempdir().map_err(|e| err(format!("seat temp dir: {e}")))?;
        let handle = AccountStoreRuntime::start(AccountRuntimeParams {
            store_root: StoreRoot::at(dir.path().join("state")),
            store_backup_exclusion: CloudBackupExclusion::NotApplicable {
                platform: "e2e harness (a throwaway store under the temp dir)".into(),
            },
            actor_id_hex: identity.actor_id_hex(),
            rpc: Arc::clone(nest),
            process_rpc: None,
            principal: RuntimePrincipal::SeedHolding(identity.clone_keypair().into()),
            credentials: fauna_credential_store::CredentialStore::with_file_backend(
                CRED_NAMESPACE,
                dir.path().join("creds"),
            ),
            reconnects: None,
            pushes: None,
            backstop_interval: DEFAULT_BACKSTOP_INTERVAL,
            memberships: None,
            trusted_escrow_holders: fixed_holders(vec![holder]),
            attested_predecessors: Default::default(),
            linked_nests: None,
            owed_nests: None,
            peer_transport,
            enrollment_target_device_id: device_id,
        })
        .await
        .map_err(|e| err(format!("start the seat's account runtime: {e:#}")))?;
        handle.settled().await;
        Ok(Self { handle, _dir: dir })
    }

    /// The seat's custody door — the one every runtime-hosting app hands the
    /// custody writers. It owns its handle (`use<>`), so a writer may hold it
    /// as an `Arc<dyn FolderKeyStore>` for as long as the seat runs.
    fn custody(
        &self,
    ) -> PlaneFolderKeys<impl Fn() -> Option<AccountStoreHandle> + Send + Sync + use<>> {
        let handle = self.handle.clone();
        PlaneFolderKeys::new(move || Some(handle.clone()))
    }

    /// Run one pass, so what the caller wrote is on the nest, and answer the
    /// pass's step failures.
    async fn publish(&self) -> Result<Vec<String>, FfiError> {
        self.handle
            .reconcile_now()
            .await
            .map(|report| report.errors)
            .map_err(|e| err(format!("the seat's publishing pass: {e:#}")))
    }

    /// Close the store; the machine's enrollment stays, as a lost device's does.
    async fn stop(self) {
        self.handle.shutdown().await;
    }
}

/// Connect as `identity`, host a [`Seat`] for the length of `op`, then close
/// it and disconnect — the scaffold every export that writes the account plane
/// runs inside, so the seat never outlives the call whatever `op` answers.
fn with_seat<T>(
    nest_url: String,
    identity: ActorKeypair,
    op: impl AsyncFnOnce(&Arc<NestClient>, &ActorKeypair, &Seat) -> Result<T, FfiError>,
) -> Result<T, FfiError> {
    let seat_identity = identity.clone_keypair();
    with_client(nest_url, identity, |nest| async move {
        let seat = Seat::start(&nest, &seat_identity).await?;
        let result = op(&nest, &seat_identity, &seat).await;
        seat.stop().await;
        result
    })
}

/// The set nonce `folder`'s records bind to, from the recorder's own roster +
/// custody ([`fauna_client_folders::record_nonce`], the read every client
/// recorder makes) — refused locally when there is none, so a harness seed can
/// never go out unsigned by accident.
///
/// **One stand-in, for a member only.** A member's production nonce arrives in
/// the set's MLS content-key envelope, which a harness member seated through a
/// synthetic Welcome never opens; for a row the roster lists as `member` the
/// nonce the nest echoes it (`FolderSummary::set_nonce`) stands in. Never for
/// an owner: an owner's set with no custody entry is exactly the
/// custody-incorrect set the owner app's launch reconcile re-mints, and it
/// fails here.
async fn set_nonce(
    nest: &Arc<NestClient>,
    identity: &ActorKeypair,
    folder: &str,
) -> Result<[u8; 32], FfiError> {
    let files = FoldersClient::new(Arc::clone(nest));
    let custody = custody_reader(nest, identity);
    let resolve =
        |e: &dyn std::fmt::Display| err(format!("resolve the set nonce of {folder:?}: {e}"));
    if let Some(nonce) = fauna_client_folders::record_nonce(&files, &*custody, folder)
        .await
        .map_err(|e| resolve(&e))?
    {
        return Ok(nonce);
    }
    let roster = files
        .list_owned_and_shared_wire()
        .await
        .map_err(|e| resolve(&e))?;
    roster
        .folders
        .iter()
        .find(|s| s.is_named(folder) && s.role.as_deref() == Some("member"))
        .and_then(|s| s.set_nonce.as_deref())
        .and_then(|n| <[u8; 32]>::try_from(n.as_slice()).ok())
        .ok_or_else(|| {
            err(format!(
                "no set nonce in the recorder's custody for {folder:?} — create the set \
                 through fauna_harness_create_set (custody first), not a raw folders.create"
            ))
        })
}

fn signing(identity: &ActorKeypair, nonce: [u8; 32]) -> RecordSigning {
    RecordSigning {
        signer: Arc::new(ChangeSigner::direct(identity)),
        set_nonce: SetNonceSource::Fixed(nonce),
    }
}

/// Run `body` (a panic becomes an error), write its [`Outcome`] into `out` and
/// return 0, or record the error and return -1.
///
/// # Safety
///
/// `out` must be a valid, aligned, writable pointer to an `FfiBuffer`.
unsafe fn run(body: impl FnOnce() -> Result<Outcome, FfiError>, out: *mut FfiBuffer) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body))
        .unwrap_or_else(|_| Err(err("panic in Rust FFI")));
    match result {
        Ok(outcome) => {
            // SAFETY: per this function's contract.
            unsafe {
                *out = FfiBuffer::from_vec(outcome.into_cbor());
            }
            0
        }
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

/// [`fauna_harness_create_set`] over a started seat: the shared create through
/// the seat's custody door, one publishing pass, then — for a set the nest
/// accepted — the proof the next reader depends on: the nonce read back off the
/// nest through [`custody_reader`], the very read a later
/// [`fauna_harness_record_change`] makes. A set whose custody stayed in the
/// seat's store would be re-minted by the owner app's launch reconcile, so
/// that is a failed call here, with the pass's step failures in the message.
async fn create_set_on(
    seat: &Seat,
    nest: &Arc<NestClient>,
    identity: &ActorKeypair,
    req: FolderCreateRequest,
) -> Result<Outcome, FfiError> {
    let files = FoldersClient::new(Arc::clone(nest));
    let name = req.name.clone();
    // Sealed from birth, as every production create is: the owner's root
    // passed explicitly, since this client carries no label custody.
    let owner = fauna_core::crypto::BackupKey::derive(identity.secret_bytes());
    let reply: Result<FolderCreateReply, NestClientError> =
        match fauna_client_folders::create_set_with_owner_root(
            &files,
            &seat.custody(),
            req,
            Some(owner),
        )
        .await
        {
            Ok(reply) => Ok(reply),
            Err(e) => match e.nest_error() {
                Some(NestClientError::Rpc(rpc)) => Err(NestClientError::Rpc(rpc.clone())),
                _ => return Err(err(format!("create_set: {e}"))),
            },
        };
    let pass_errors = seat.publish().await?;
    if reply.is_ok() {
        let published =
            fauna_client_folders::record_nonce(&files, &*custody_reader(nest, identity), &name)
                .await
                .map_err(|e| err(format!("read {name:?}'s custody back off the nest: {e}")))?;
        if published.is_none() {
            return Err(err(format!(
                "the set {name:?} was created, but its custody entry did not reach the nest — \
                 the seat's publishing pass reported: {pass_errors:?}"
            )));
        }
    }
    outcome!(reply)
}

/// Create a set **custody-first** as the actor `secret` names, through the
/// shared [`fauna_client_folders::create_set`]: its nonce minted into the
/// owner's custody — the account plane's, through a [`Seat`] hosted for this
/// call — then `fauna.folders.create` carrying it.
///
/// `request` is a dag-cbor `FolderCreateRequest` (any `set_nonce` in it is
/// overwritten — the nonce is the helper's to mint). `out` receives
/// `{"ok": FolderCreateReply}` or `{"err": RpcError}`.
///
/// # Safety
///
/// `nest_url` must be a valid NUL-terminated C string; `secret` / `request`
/// must point to `secret_len` / `request_len` readable bytes; `out` must be a
/// valid, aligned, writable pointer to an `FfiBuffer`. All pointers must stay
/// valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_create_set(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let req: FolderCreateRequest = decode_request!(call.req)?;
        with_seat(
            call.nest_url,
            call.identity,
            async |nest, identity, seat| create_set_on(seat, nest, identity, req).await,
        )
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Record one change **signed**, as the actor `secret` names: the set's nonce
/// read from that actor's roster + custody, the request signed through the
/// shared [`RecordSigning`] with the identity key, then
/// `fauna.sync.changes.record`.
///
/// `request` is a dag-cbor `SyncChangeRecordRequest` carrying no signature
/// fields (they are this export's to fill). `out` receives
/// `{"ok": SyncChangeRecordReply}` or `{"err": RpcError}`.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_record_change(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let mut req: SyncChangeRecordRequest = decode_request!(call.req)?;
        let identity = call.identity;
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            let nonce = set_nonce(&nest, &identity, &req.folder).await?;
            signing(&identity, nonce).sign(&mut req).await;
            if req.signature.is_none() {
                return Err(err("the change record did not sign (see the log)"));
            }
            // By hash, through the one funnel every app's record takes: a sealed
            // set's row rests no plaintext name (schema 114), so a by-name
            // record finds no set. The signature binds no address field.
            let reply: Result<SyncChangeRecordReply, NestClientError> = nest
                .request(
                    "fauna.sync.changes.record",
                    fauna_protocol::folders::addressed(req),
                )
                .await;
            outcome!(reply)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Report a conflict as the actor `secret` names, a resolved report's winner
/// head row **signed** through the shared [`RecordSigning::sign_report`] under
/// the set's nonce, then `fauna.sync.conflicts.report`. An unresolved report
/// mints no row and goes out as it came.
///
/// `request` is a dag-cbor `ConflictReportRequest`. `out` receives
/// `{"ok": ConflictReportReply}` or `{"err": RpcError}`.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_report_conflict(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let mut req: ConflictReportRequest = decode_request!(call.req)?;
        let identity = call.identity;
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            if req.resolution.is_some() {
                let nonce = set_nonce(&nest, &identity, &req.folder).await?;
                signing(&identity, nonce).sign_report(&mut req).await;
                // Both rows the report mints are signed — the retained loser's
                // too, whenever there is one (ruling (10)(d)).
                let retains_loser =
                    fauna_protocol::sync_writer_sig::SignedChange::for_retained_loser(
                        &req,
                        identity.actor_id().0,
                        nonce,
                    )
                    .map_err(|e| err(format!("the resolved report's retained loser: {e}")))?
                    .is_some();
                if req.winner_signature.is_none()
                    || (retains_loser && req.loser_signature.is_none())
                {
                    return Err(err("the resolved report did not sign (see the log)"));
                }
            }
            let reply: Result<ConflictReportReply, NestClientError> = nest
                .request(
                    "fauna.sync.conflicts.report",
                    fauna_protocol::folders::addressed(req),
                )
                .await;
            outcome!(reply)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// One **seat pass** over a real directory, as the actor `secret` names — the
/// harness's signed file WRITER where a test has no app to write through.
///
/// Builds the shared engine for the set `folder_id` (a `FolderRef` wire string)
/// over `watch_dir` exactly as the apps' in-process host does
/// ([`crate::sync_engine_host::host_context`] + `build_engine`, the identity
/// seed as its credential, so every record signs directly with the identity key
/// — `effective_change_signer`), runs ONE local converge (drain, reconcile,
/// upload: new and changed files go up sealed and recorded, a file gone since
/// the last pass is recorded deleted), then drops the engine. The set must be
/// custody-correct ([`fauna_harness_create_set`]): the engine reads its nonce
/// from custody and signs nothing without one. The engine's state DB lives in
/// `state_dir`, so successive passes over one directory see its history — the
/// shape a delete needs. It never pulls: a reader is the test's other seat.
///
/// `device_id` (32 bytes) is the device the pass registers under
/// `device_label` and records as. `out` receives `{"ok": [recorded, pending]}`:
/// the rels whose record reached the nest in this pass, and how many files the
/// pass left pending (non-zero means the pass did not finish — a test says so
/// rather than waiting on a reader for a file that never went up).
///
/// # Safety
///
/// `nest_url`, `folder_id`, `watch_dir`, `state_dir` and `device_label` must be
/// valid NUL-terminated C strings; `secret` / `device_id` must point to
/// `secret_len` / `device_id_len` readable bytes; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`. All pointers must stay valid
/// for the duration of the call.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn fauna_harness_sync_pass(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    folder_id: *const c_char,
    watch_dir: *const c_char,
    state_dir: *const c_char,
    device_id: *const u8,
    device_id_len: u32,
    device_label: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let (call, folder_id, watch_dir, state_dir, device_id, device_label) = unsafe {
            (
                decode_call(nest_url, secret, secret_len, std::ptr::null(), 0)?,
                cstr_to_string(folder_id)?,
                cstr_to_string(watch_dir)?,
                cstr_to_string(state_dir)?,
                slice_to_vec(device_id, device_id_len),
                cstr_to_string(device_label)?,
            )
        };
        let device_id: [u8; 32] = device_id.as_slice().try_into().map_err(|_| {
            err(format!(
                "device_id must be exactly 32 bytes, got {}",
                device_id.len()
            ))
        })?;
        let folder_ref = crate::sync_engine_host::parse_folder_id(&folder_id)?;
        let seed = *call.identity.secret_bytes();
        let identity = call.identity;
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            let folder_keys = custody_reader(&nest, &identity);
            let ctx = crate::sync_engine_host::host_context(
                state_dir,
                seed,
                device_id,
                device_label,
                nest,
                None,
                folder_keys,
                Vec::new(),
            );
            let params = ctx.params(std::path::PathBuf::from(watch_dir), folder_ref, None);
            let built = fauna_sync_engine::engine_lifecycle::build_engine(params)
                .await
                .ok_or_else(|| {
                    err(format!(
                        "folder {folder_id}: the engine refused to build (binding \
                         indeterminate, or the set is gone) — see the log"
                    ))
                })?;
            let recorded = fauna_sync_engine::always_resident::LocalWriteHost::converge(
                &built.engine,
                &folder_id,
            )
            .await;
            let pending = built
                .engine
                .db()
                .transfer_backlog()
                .map_err(|e| err(format!("read the pass's backlog: {e}")))?
                .files_pending;
            fauna_core::encoding::canonical_encode(&(recorded, pending))
                .map(Outcome::Ok)
                .map_err(encode_err)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// A second keypair over the same seed — [`ActorKeypair`] is deliberately not
/// `Clone`, and the connection, the seat's runtime and the signer each take
/// one.
trait CloneKeypair {
    fn clone_keypair(&self) -> ActorKeypair;
}

impl CloneKeypair for ActorKeypair {
    fn clone_keypair(&self) -> ActorKeypair {
        ActorKeypair::from_secret(*self.secret_bytes())
    }
}

/// Build one **NIP-17 gift-wrapped DM** as a far Nostr user would: `content`
/// from the holder of `sender_secret` to `recipient` (the account's key as
/// `fauna.bridges.list` shows it — `npub1…` — or 64 hex), through the same
/// [`fauna_bridge_nostr::nip17::wrap_dm`] the nest's own outbound leg seals
/// with — the honest three layers (kind-14 rumor, kind-13 seal, kind-1059
/// wrap under a throwaway key). The e2e harness posts the result to a nest's
/// `/nostr` relay endpoint as an unauthenticated `EVENT`, which is how a real
/// client's DM reaches an account's custodial key (`ui/nostr.md`
/// § Implementation status today → DMs). Not networked: it only builds the
/// event; `out` receives it as NIP-01 JSON.
///
/// # Safety
///
/// `sender_secret` must point to 32 readable bytes; `recipient` and `content`
/// must be valid NUL-terminated C strings; `out` must be a
/// valid, aligned, writable pointer to an `FfiBuffer`. All pointers must stay
/// valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_nostr_gift_wrap_dm(
    sender_secret: *const u8,
    recipient: *const c_char,
    content: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || -> Result<Vec<u8>, FfiError> {
        // SAFETY: per the `# Safety` section above.
        let (secret, recipient, content) = unsafe {
            (
                super::read_32(sender_secret, "sender_secret")?,
                cstr_to_string(recipient)?,
                cstr_to_string(content)?,
            )
        };
        let recipient = match fauna_bridge_nostr::nip19::decode_npub(&recipient) {
            Ok(bytes) => bytes,
            Err(_) => fauna_core::hex32::decode(&recipient)
                .map_err(|e| err(format!("recipient is neither an npub nor 64 hex: {e}")))?,
        };
        let sender = fauna_bridge_nostr::signing::Keypair::from_secret_bytes(secret)
            .map_err(|e| err(format!("sender_secret is not a secp256k1 secret key: {e}")))?;
        let wrap = fauna_bridge_nostr::nip17::wrap_dm(&sender, &recipient, &content)
            .map_err(|e| err(format!("gift wrap: {e}")))?;
        serde_json::to_vec(&wrap).map_err(|e| err(format!("encode the event: {e}")))
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { write_out(body(), out) }
}

/// The Nostr exports' one reply path: the bytes into `out` and `0`, or the
/// message into `fauna_ffi_last_error` and `-1`.
///
/// # Safety
///
/// `out` must be a valid, aligned, writable pointer to an `FfiBuffer`.
unsafe fn write_out(result: Result<Vec<u8>, FfiError>, out: *mut FfiBuffer) -> i32 {
    match result {
        Ok(bytes) => {
            // SAFETY: per the `# Safety` section above.
            unsafe {
                *out = FfiBuffer::from_vec(bytes);
            }
            0
        }
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

/// Sign `template` — a NIP-01 event without `pubkey`/`id`/`sig` (`kind`,
/// `created_at`, `tags`, `content`), the shape NIP-07's `signEvent` takes and
/// `fauna.bridges.link_challenge` hands out — as the holder of `secret`,
/// through the shared [`fauna_bridge_nostr::signing::Keypair::sign_event`]. It
/// is the harness's stand-in for any Nostr key held OUTSIDE a nest: a browser
/// extension proving a key at link time, or a far author whose notes and
/// deletions a fake relay serves. `out` receives the signed event as NIP-01
/// JSON; a `pubkey` in `template` is ignored (the key decides it).
///
/// # Safety
///
/// `secret` must point to 32 readable bytes; `template` must be a valid
/// NUL-terminated C string; `out` must be a valid, aligned, writable pointer to
/// an `FfiBuffer`. All pointers must stay valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_nostr_sign_event(
    secret: *const u8,
    template: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || -> Result<Vec<u8>, FfiError> {
        // SAFETY: per the `# Safety` section above.
        let (secret, template) =
            unsafe { (super::read_32(secret, "secret")?, cstr_to_string(template)?) };
        let keypair = fauna_bridge_nostr::signing::Keypair::from_secret_bytes(secret)
            .map_err(|e| err(format!("secret is not a secp256k1 secret key: {e}")))?;
        let t: serde_json::Value = serde_json::from_str(&template)
            .map_err(|e| err(format!("template is not JSON: {e}")))?;
        let tags: Vec<fauna_bridge_nostr::types::Tag> = serde_json::from_value(
            t.get("tags")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )
        .map_err(|e| err(format!("template tags: {e}")))?;
        let unsigned = fauna_bridge_nostr::types::UnsignedEvent {
            pubkey: keypair.public_key_bytes(),
            created_at: t["created_at"]
                .as_u64()
                .ok_or_else(|| err("template has no integer created_at"))?,
            kind: t["kind"]
                .as_u64()
                .ok_or_else(|| err("template has no integer kind"))?,
            tags,
            content: t["content"].as_str().unwrap_or_default().to_string(),
        };
        serde_json::to_vec(&keypair.sign_event(unsigned))
            .map_err(|e| err(format!("encode the event: {e}")))
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { write_out(body(), out) }
}

/// Check `event` (NIP-01 JSON) as any Nostr client does before trusting it:
/// the id is the canonical serialization's hash and the schnorr signature is
/// the pubkey's, through the shared [`fauna_bridge_nostr::signing::verify_event`].
/// `out` receives `true` or `false` as JSON; an `event` that is not a NIP-01
/// event at all is a local failure (`-1`).
///
/// # Safety
///
/// `event` must be a valid NUL-terminated C string; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`. Both must stay valid for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_nostr_verify_event(
    event: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || -> Result<Vec<u8>, FfiError> {
        // SAFETY: per the `# Safety` section above.
        let event = unsafe { cstr_to_string(event)? };
        let event: fauna_bridge_nostr::types::Event =
            serde_json::from_str(&event).map_err(|e| err(format!("not a NIP-01 event: {e}")))?;
        let valid = fauna_bridge_nostr::signing::verify_event(&event);
        Ok(if valid {
            b"true".to_vec()
        } else {
            b"false".to_vec()
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { write_out(body(), out) }
}

/// The NIP-19 faces of the key `secret` holds — `{"pubkey": <64 hex>,
/// "npub": "npub1…", "nsec": "nsec1…"}` — through the shared
/// [`fauna_bridge_nostr::nip19`] codec: what a user pastes to import a key,
/// and what `fauna.bridges.list` shows once it is linked.
///
/// # Safety
///
/// `secret` must point to 32 readable bytes; `out` must be a valid, aligned,
/// writable pointer to an `FfiBuffer`. Both must stay valid for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_nostr_keys(secret: *const u8, out: *mut FfiBuffer) -> i32 {
    let body = || -> Result<Vec<u8>, FfiError> {
        // SAFETY: per the `# Safety` section above.
        let secret = unsafe { super::read_32(secret, "secret")? };
        let keypair = fauna_bridge_nostr::signing::Keypair::from_secret_bytes(secret)
            .map_err(|e| err(format!("secret is not a secp256k1 secret key: {e}")))?;
        let pubkey = keypair.public_key_bytes();
        serde_json::to_vec(&serde_json::json!({
            "pubkey": hex::encode(pubkey),
            "npub": fauna_bridge_nostr::nip19::encode_npub(&pubkey),
            "nsec": fauna_bridge_nostr::nip19::encode_nsec(&secret),
        }))
        .map_err(|e| err(format!("encode the keys: {e}")))
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { write_out(body(), out) }
}

/// The 64-hex public key an `npub1…` names, through the shared
/// [`fauna_bridge_nostr::nip19::decode_npub`] — how a test turns the identity
/// `fauna.bridges.list` shows into the `authors` a relay filter takes. `out`
/// receives the hex as a JSON string.
///
/// # Safety
///
/// `npub` must be a valid NUL-terminated C string; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`. Both must stay valid for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_nostr_npub_to_hex(
    npub: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || -> Result<Vec<u8>, FfiError> {
        // SAFETY: per the `# Safety` section above.
        let npub = unsafe { cstr_to_string(npub)? };
        let bytes = fauna_bridge_nostr::nip19::decode_npub(&npub)
            .map_err(|e| err(format!("not an npub: {e}")))?;
        serde_json::to_vec(&hex::encode(bytes)).map_err(|e| err(format!("encode: {e}")))
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { write_out(body(), out) }
}

/// Sign the owner's **custody witness** — the `CustodyGrant` a custodian
/// presents at `fauna.custody.hosting.register` and at the owner nest's custody
/// handshake — as the account whose seed is `secret`, through the shared
/// [`fauna_core::custody_grant::sign_custody_grant`] (`account-replica-posture.md`
/// § The custody grant + ceremony). The scope set is the whole account
/// ([`CustodyScopeSet::Account`]), the grant is minted at the fixed stamp the
/// conformance rigs use (`Timestamp(1_000)`) and expires at `expires_at_us`
/// (microseconds, the `Timestamp` unit), and it names `removed_devices` as
/// removed — the concatenated 32-byte device keys: none for the shape the
/// ceremony's `deliver` hands a host, some for the re-minted grant a device
/// removal deposits (the serve door's exclusion, ruling 4). Not networked —
/// the harness's stand-in for the owner's half of a ceremony a test needs only
/// the signed bytes of.
///
/// `out` receives the witness's canonical bytes verbatim (the
/// `HostingRegisterRequest.witness` field).
///
/// # Safety
///
/// `secret` and `custodian_key` must point to 32 readable bytes,
/// `grant_id` to `grant_id_len` readable bytes and `removed_devices` to
/// `removed_devices_len` readable bytes (a multiple of 32; null with a zero
/// length is none); `out` must be a valid, aligned, writable pointer to an
/// `FfiBuffer`. All pointers must stay valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_sign_custody_witness(
    secret: *const u8,
    grant_id: *const u8,
    grant_id_len: u32,
    custodian_key: *const u8,
    expires_at_us: u64,
    removed_devices: *const u8,
    removed_devices_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_core::custody_grant::{CustodyGrant, CustodyScopeSet, sign_custody_grant};
    use fauna_core::data::Timestamp;

    let body = || -> Result<Vec<u8>, FfiError> {
        // SAFETY: per the `# Safety` section above.
        let (secret, grant_id, custodian_key) = unsafe {
            (
                super::read_32(secret, "secret")?,
                slice_to_vec(grant_id, grant_id_len),
                super::read_32(custodian_key, "custodian_key")?,
            )
        };
        // SAFETY: per the `# Safety` section above.
        let removed = unsafe { slice_to_vec(removed_devices, removed_devices_len) };
        if removed.len() % 32 != 0 {
            return Err(err("removed_devices must be whole 32-byte device keys"));
        }
        let removed_devices: Vec<[u8; 32]> = removed.as_chunks::<32>().0.to_vec();
        let owner = ActorKeypair::from_secret(secret);
        let witness = sign_custody_grant(
            &owner,
            &CustodyGrant {
                grant_id,
                owner: owner.actor_id(),
                custodian_key,
                scopes: CustodyScopeSet::Account,
                minted_at: Timestamp(1_000),
                expires_at: Timestamp(expires_at_us),
                removed_devices,
            },
        )
        .map_err(|e| err(format!("sign the custody witness: {e}")))?;
        fauna_core::encoding::canonical_encode(&witness).map_err(encode_err)
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { write_out(body(), out) }
}

/// Mint the **custody capability row** on the owner's own nest, as the account
/// whose seed is `secret` — the owner's half of a custody a host then deposits
/// a hosting row for (`account-replica-posture.md` § The custody grant +
/// ceremony). The ceremony's own mint order, over the seat's real ledger door
/// (`fauna_client_capabilities::custody_ceremony`'s mint step): the signed
/// `Mint` event recorded and published through the account runtime
/// ([`fauna_client_config::SuccessionLedgerStore::merge_published`] — the
/// bound nest acknowledges it), the keyless blob built by the shared
/// [`fauna_client_capabilities::custody_grants::custody_mint_blob`] and
/// released only against that published log, then `fauna.capabilities.mint`.
/// Whole-account scope ([`fauna_core::custody_grant::CustodyScopeSet::Account`]),
/// as [`fauna_harness_sign_custody_witness`] signs.
///
/// `request` is the dag-cbor array `[grant_id, custodian_key, window_start,
/// window_end]` (bytes, 32 bytes, seconds, seconds). `out` receives
/// `{"ok": MintGrantReply}` or `{"err": RpcError}`.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_mint_custody(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_capabilities::custody_grants::{custody_event_scopes, custody_mint_blob};
    use fauna_client_capabilities::grant_log::{self, PublishedGrants};
    use fauna_client_capabilities::rpc::CapabilitiesClient;
    use fauna_client_config::SuccessionLedgerStore;
    use fauna_core::custody_grant::{CUSTODY_GRANT_ID_LEN, CustodyScopeSet};
    use fauna_core::succession_ledger::SuccessionLedger;
    use fauna_mls::wrapped_blob::GrantWindow;

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (grant_id, custodian_key, window_start, window_end): (
            fauna_protocol::ByteBuf,
            fauna_protocol::ByteBuf,
            u64,
            u64,
        ) = decode_request!(call.req)?;
        let grant_id: [u8; CUSTODY_GRANT_ID_LEN] = grant_id
            .as_slice()
            .try_into()
            .map_err(|_| err(format!("grant_id must be {CUSTODY_GRANT_ID_LEN} bytes")))?;
        let custodian_key: [u8; 32] = custodian_key
            .as_slice()
            .try_into()
            .map_err(|_| err("custodian_key must be 32 bytes"))?;
        let identity = call.identity;
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            let set = CustodyScopeSet::Account;
            let now = fauna_core::data::Timestamp::now().0 / 1_000_000;
            let mut intent = SuccessionLedger::events_replica(identity.actor_id(), Vec::new());
            grant_log::record_mint(
                &mut intent,
                identity.signing_key(),
                grant_id,
                custodian_key,
                custody_event_scopes(&set),
                window_start,
                window_end,
                now,
            )
            .map_err(|e| err(format!("record the Mint event: {e}")))?;
            let seat = Seat::start(&nest, &identity).await?;
            let published = SuccessionLedgerStore::merge_published(&seat.handle, intent).await;
            seat.stop().await;
            let published = published.map_err(|e| err(format!("publish the grant log: {e}")))?;
            let blob = custody_mint_blob(
                &identity.actor_id().0,
                &grant_id,
                &custodian_key,
                GrantWindow(window_start, window_end),
                &set,
            )
            .map_err(|e| err(format!("build the custody blob: {e}")))?
            .release(&PublishedGrants::from_published(&published))
            .map_err(|e| err(e.to_string()))?;
            outcome!(CapabilitiesClient::new(Arc::clone(&nest)).mint(blob).await)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Revoke a custody the owner whose seed is `secret` minted, through the shared
/// [`fauna_client_capabilities::custody_acts::revoke_custody`] — the nest's
/// `fauna.capabilities.revoke` first, then the signed `Revoke` event recorded
/// over the seat's ledger door, the one order every app's revoke control takes.
///
/// `request` is the dag-cbor array `[grant_id, holder]` (bytes, the 32-byte
/// custodian key the grant names). `out` receives `{"ok": true}`; a refusal
/// is a local failure carrying the act's own error text.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_revoke_custody(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (grant_id, holder): (fauna_protocol::ByteBuf, fauna_protocol::ByteBuf) =
            decode_request!(call.req)?;
        let holder: [u8; 32] = holder
            .as_slice()
            .try_into()
            .map_err(|_| err("holder must be 32 bytes"))?;
        let identity = call.identity;
        let secret = *identity.secret_bytes();
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            let seat = Seat::start(&nest, &identity).await?;
            let failed = fauna_client_capabilities::custody_acts::revoke_custody(
                Arc::clone(&nest),
                &seat.handle,
                secret,
                &grant_id,
                Some(holder),
            )
            .await;
            seat.stop().await;
            match failed {
                None => fauna_core::encoding::canonical_encode(&true)
                    .map(Outcome::Ok)
                    .map_err(encode_err),
                Some(e) => Err(err(format!("revoke the custody: {e}"))),
            }
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// One **custodied-fleet handshake** at a custodian nest — an owner-fleet
/// device asking the nest that holds a custodied copy of its owner's account
/// for a restore-read bearer (`account-replica-posture.md` § Replica posture →
/// *The custody grant + ceremony*, the custodian-NEST serve door bullet,
/// ruling 2), through the shared
/// [`fauna_anon_client::mint_bearer_over_custodied_fleet_handshake`]. The
/// account whose seed is `secret` is the OWNER the device claims; the device's
/// root-signed `DeviceAuthorization` is minted here over the device key and
/// signed by the owner — or by `cert_signer` when given, the shape of a device
/// another account vouches for (a stranger's, or the host's own).
///
/// `request` is the dag-cbor array `[device_secret, cert_signer]` (the
/// device's 32-byte Ed25519 seed; another account's 32-byte seed, or null for
/// the owner). `out` receives `{"ok": token}` — the bearer a test presents on
/// its own WS connection, so it can hold a session open across a revocation —
/// or `{"err": RpcError}`, the nest's refusal.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_custodied_fleet_handshake(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, sign_envelope};

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (device_secret, cert_signer): (
            fauna_protocol::ByteBuf,
            Option<fauna_protocol::ByteBuf>,
        ) = decode_request!(call.req)?;
        let seed = |b: &[u8], what: &str| -> Result<[u8; 32], FfiError> {
            b.try_into()
                .map_err(|_| err(format!("{what} must be a 32-byte seed")))
        };
        let device = ed25519_dalek::SigningKey::from_bytes(&seed(&device_secret, "device_secret")?);
        let signer = match cert_signer {
            Some(other) => ActorKeypair::from_secret(seed(&other, "cert_signer")?),
            None => call.identity.clone_keypair(),
        };
        let cert = DeviceAuthorization {
            actor_id: signer.actor_id(),
            device_key: device.verifying_key().to_bytes(),
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) =
            sign_envelope(&signer, &cert).map_err(|e| err(format!("sign the cert: {e}")))?;
        let cert = EmbedAsBytes::from_signed(bytes, env);
        let owner = call.identity.actor_id().0;
        runtime()?.block_on(async move {
            match fauna_anon_client::mint_bearer_over_custodied_fleet_handshake(
                &call.nest_url,
                owner,
                &device,
                &cert,
            )
            .await
            {
                Ok(minted) => fauna_core::encoding::canonical_encode(&minted.token)
                    .map(Outcome::Ok)
                    .map_err(encode_err),
                Err(fauna_anon_client::AnonClientError::Rpc(e)) => {
                    fauna_core::encoding::canonical_encode(&e)
                        .map(Outcome::Err)
                        .map_err(encode_err)
                }
                Err(e) => Err(err(format!("custodied-fleet handshake: {e}"))),
            }
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Make a **private (fragment-keyed) share link** to a file, as the actor
/// `secret` names, through the shared
/// [`fauna_client_share::ShareClient::create_private_link`] — the mint, the
/// key envelope sealed under a fresh link key, the registration, and the URL
/// revealed only against the nest's reply (`share-links.md` § The private-file
/// extension). The fixture for a viewer journey until an app's create control
/// routes to the private arm; never a re-derivation of the token or envelope.
///
/// `request` is the dag-cbor array `[manifest, filename, lifetime_secs,
/// names]`: `manifest` the file's canonical `ChunkManifest` in its sealed wire
/// form (the bytes uploaded to `POST /api/v1/manifests`, sealed under this
/// owner's root — `fauna_folder_seal_owner_file`), `filename` the name the
/// envelope carries. `names` is null for the conforming link; a 32-byte
/// manifest hash there makes the token name THAT manifest while the envelope
/// is still built over `manifest` — the shape only a non-conforming client
/// sends, which a witness of the nest's own create-time refusal needs (the
/// shared mint refuses a plaintext manifest before any request leaves).
/// `out` receives `{"ok": [ShareRecord, url]}` or `{"err": RpcError}`.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_create_private_link(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (manifest_bytes, filename, lifetime_secs, names): (
            fauna_protocol::ByteBuf,
            String,
            u64,
            Option<fauna_protocol::ByteBuf>,
        ) = decode_request!(call.req)?;
        let manifest: fauna_core::chunk::ChunkManifest =
            decode_request!(manifest_bytes.as_slice())?;
        let manifest_hash = match names {
            Some(other) => other
                .as_slice()
                .try_into()
                .map_err(|_| err("names must be a 32-byte manifest hash"))?,
            None => fauna_core::data::ContentHash::of_raw(&manifest_bytes).digest(),
        };
        let file = fauna_client_share::LinkFile {
            manifest_hash,
            filename,
        };
        let author =
            fauna_client_share::ShareAuthor::new(*call.identity.secret_bytes(), &call.nest_url);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| err(format!("clock: {e}")))?
            .as_secs();
        let identity = call.identity;
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            let client = fauna_client_share::ShareClient::new(nest);
            match client
                .create_private_link(&author, &file, manifest, lifetime_secs, now)
                .await
            {
                Ok(created) => outcome!(Ok::<_, NestClientError>(created)),
                Err(fauna_client_share::CreateError::Rpc(e)) => outcome!(Err::<(), _>(e)),
                Err(fauna_client_share::CreateError::Mint(e)) => Err(err(format!("mint: {e}"))),
                Err(fauna_client_share::CreateError::Mismatch) => {
                    Err(err("the nest answered for another token"))
                }
            }
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Make an ordinary (not fragment-keyed) **share link** to a file, as the
/// actor `secret` names, through the shared
/// [`fauna_client_share::ShareClient::create_link`] — the mint, the
/// registration, and the URL revealed only against the nest's reply
/// (`share-links.md` § What a link is). The twin of
/// [`fauna_harness_create_private_link`] for the link with no key — the
/// fixture a witness of the create-time refusals (§ Which files can be
/// linked) needs, since a sealed file's ordinary link is refused there.
///
/// `request` is the dag-cbor array `[manifest_hash, filename, lifetime_secs]`:
/// `manifest_hash` the 32-byte hash of the manifest as uploaded. `out`
/// receives `{"ok": [ShareRecord, url]}` or `{"err": RpcError}`.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_create_link(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (manifest_hash, filename, lifetime_secs): (fauna_protocol::ByteBuf, String, u64) =
            decode_request!(call.req)?;
        let manifest_hash: [u8; 32] = manifest_hash
            .as_slice()
            .try_into()
            .map_err(|_| err("manifest_hash must be 32 bytes"))?;
        let file = fauna_client_share::LinkFile {
            manifest_hash,
            filename,
        };
        let author =
            fauna_client_share::ShareAuthor::new(*call.identity.secret_bytes(), &call.nest_url);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| err(format!("clock: {e}")))?
            .as_secs();
        let identity = call.identity;
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            let client = fauna_client_share::ShareClient::new(nest);
            match client.create_link(&author, &file, lifetime_secs, now).await {
                Ok(created) => outcome!(Ok::<_, NestClientError>(created)),
                Err(fauna_client_share::CreateError::Rpc(e)) => outcome!(Err::<(), _>(e)),
                Err(fauna_client_share::CreateError::Mint(e)) => Err(err(format!("mint: {e}"))),
                Err(fauna_client_share::CreateError::Mismatch) => {
                    Err(err("the nest answered for another token"))
                }
            }
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Open a **private share link as its viewer page does** — the stranger's
/// side, with no browser: the link's address read by
/// [`fauna_client_share::viewer::viewer_start`], the manifest-path answer
/// opened by [`fauna_client_share::viewer::open_share`] (the token's
/// signature, the manifest the token names, the envelope under the fragment's
/// key), and the chunks assembled by
/// [`fauna_client_share::viewer::OpenedShare::assemble`] (each against its
/// plaintext hash, the whole against the file hash). The same functions the
/// viewer's wasm (`libs/fauna-wasm-share`) runs; the HTTP stays the caller's,
/// so what a test fetches is exactly what a stranger's browser would.
/// Not networked.
///
/// `request` is the dag-cbor array `[url, manifest_reply, chunks]`: `url` the
/// whole link (`<nest>/share/<token>#<key>`), `manifest_reply` the body of
/// `GET <link path>/manifest`, `chunks` the bodies of `GET <link
/// path>/chunk/<i>` in index order — or `null` to learn how many to fetch.
/// `out` receives the dag-cbor array `[filename, chunk_count, plaintext]`,
/// `plaintext` `null` when `chunks` was. A link that does not open (not a
/// link, a wrong key, a damaged answer) is a non-zero return with the
/// viewer's refusal in `fauna_ffi_last_error`.
///
/// # Safety
///
/// `request` must point to `request_len` readable bytes; `out` must be a
/// valid, aligned, writable pointer to an `FfiBuffer`. Both must stay valid
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_share_viewer_open(
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_share::viewer::{ViewerStart, open_share, viewer_start};
    let body = || -> Result<Vec<u8>, FfiError> {
        // SAFETY: per the `# Safety` section above.
        let request = unsafe { slice_to_vec(request, request_len) };
        let (url, manifest_reply, chunks): (
            String,
            fauna_protocol::ByteBuf,
            Option<Vec<fauna_protocol::ByteBuf>>,
        ) = decode_request!(request)?;
        let (path, hash) = url.split_once('#').unwrap_or((url.as_str(), ""));
        let pathname = path
            .find("/share/")
            .map(|i| &path[i..])
            .ok_or_else(|| err("the url has no /share/ path"))?;
        let ViewerStart::Open { token, fragment } = viewer_start(pathname, hash) else {
            return Err(err(
                "viewer: the url is not a link to open (no token or no key)",
            ));
        };
        let opened = open_share(&token, &fragment, &manifest_reply)
            .map_err(|e| err(format!("viewer: {e:?}")))?;
        let plaintext = match chunks {
            None => None,
            Some(chunks) => {
                let ciphertexts: Vec<Vec<u8>> = chunks.into_iter().map(|c| c.into_vec()).collect();
                Some(fauna_protocol::ByteBuf::from(
                    opened
                        .assemble(&ciphertexts)
                        .map_err(|e| err(format!("viewer: {e:?}")))?,
                ))
            }
        };
        fauna_core::encoding::canonical_encode(&(
            opened.filename().to_string(),
            opened.chunk_count() as u64,
            plaintext,
        ))
        .map_err(encode_err)
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body))
        .unwrap_or_else(|_| Err(err("panic in Rust FFI")))
    {
        Ok(bytes) => {
            // SAFETY: per the `# Safety` section above, `out` is a valid,
            // aligned, writable pointer to an `FfiBuffer`.
            unsafe {
                *out = FfiBuffer::from_vec(bytes);
            }
            0
        }
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

/// One whole mailbox import as an app-less client: the shared
/// [`fauna_client_mail_settings::MailImportMachine`], built by the same
/// `rpc_glue::build_mail_import_machine` every native app builds it with,
/// walked Source → Scope → Confirm → Start with its drive loop run to the end,
/// over a Generic IMAP source at `host`:`port` (implicit TLS, every mailbox the
/// source lists, no since date, the default size limit). It is a witness's door
/// to the claims `mailbox-migration.md` makes of the CLIENT rather than of any
/// app's UI — § Credential handling's *the nest never receives the source
/// credentials* is the first — so nothing here re-derives a request: the
/// password goes into the machine's `SetPassword` and nowhere else, exactly as
/// an app hands it over.
///
/// The source must be one this process trusts: a harness server's CA reaches
/// the native connector through the e2e source-trust seed, which the caller
/// sets in this process's environment before the call (`fauna_ffi.py`'s
/// `harness_mail_import` does).
///
/// `out` receives `{"ok": [step, imported, skipped, errored, error, log]}` —
/// the machine's final snapshot: the wizard step it ended on (`"Done"` for a
/// finished import), its three counters, the `error-message` text (empty for
/// none) and the error log. A walk that stopped early (a refused connect, a
/// refused start) is an `ok` reply naming the step it stopped on; a non-zero
/// return is a local failure only (bad arguments, no connection to the nest).
///
/// # Safety
///
/// `nest_url`, `host`, `username` and `password` must be valid NUL-terminated
/// C strings; `secret` must point to `secret_len` readable bytes; `out` must be
/// a valid, aligned, writable pointer to an `FfiBuffer`. All pointers must stay
/// valid for the duration of the call.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn fauna_harness_mail_import(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    host: *const c_char,
    port: u16,
    username: *const c_char,
    password: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_mail_settings::{
        DEFAULT_MAX_SIZE_MB, ImportSourceKind, ImportStep, ImportTlsMode, MailImportAction,
        connect_actions, scope_next_actions,
    };
    use fauna_core::secret::SecretString;

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let (call, host, username, password) = unsafe {
            (
                decode_call(nest_url, secret, secret_len, std::ptr::null(), 0)?,
                cstr_to_string(host)?,
                cstr_to_string(username)?,
                SecretString::new(cstr_to_string(password)?),
            )
        };
        with_client(call.nest_url, call.identity, |nest| async move {
            let machine = fauna_client_mail_settings::rpc_glue::build_mail_import_machine(nest);
            // Each step's refusal lands on the snapshot (`error`, the step it
            // stayed on), which is what the reply reports — so a dispatch's
            // own `Err` carries nothing more, and the walk stops there.
            let walk = async {
                let source = [
                    MailImportAction::SelectSourceKind {
                        kind: ImportSourceKind::Generic,
                    },
                    MailImportAction::SetTlsMode {
                        mode: ImportTlsMode::Implicit,
                    },
                ];
                let connect = connect_actions(
                    ImportSourceKind::Generic,
                    &host,
                    &port.to_string(),
                    &username,
                    password,
                );
                for action in source.into_iter().chain(connect) {
                    machine.dispatch(action).await.ok()?;
                }
                for action in scope_next_actions("", DEFAULT_MAX_SIZE_MB) {
                    machine.dispatch(action).await.ok()?;
                }
                machine.dispatch(MailImportAction::Start).await.ok()?;
                if machine.snapshot().step == ImportStep::Progress {
                    let _ = machine.run_import().await;
                }
                Some(())
            };
            walk.await;
            let snap = machine.snapshot();
            let reply = (
                format!("{:?}", snap.step),
                snap.imported_count,
                snap.skipped_count,
                snap.errored_count,
                snap.error.unwrap_or_default(),
                snap.error_log,
            );
            fauna_core::encoding::canonical_encode(&reply)
                .map(Outcome::Ok)
                .map_err(encode_err)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// [`fauna_harness_mail_import`]'s Outlook twin: the same machine, built the
/// same way, walked as the Outlook source signs in — `Refresh` (which adopts
/// the deployment's Microsoft registration off `fauna.setup.status`, as an
/// opened page does), Outlook picked, then [`microsoft_sign_in_actions`] for
/// `username` and, once the code is out, `run_microsoft_sign_in` until the
/// grant has connected — then Scope (every mailbox, no since date, the default
/// size limit) → Confirm → Start with the drive loop run to the end. It is the
/// door for the claims `mailbox-migration.md` § Microsoft sign-in — the
/// device-code grant makes of the CLIENT: the token lives in the machine's
/// private state and never reaches the nest.
///
/// The call blocks while the sign-in polls: the caller approves the code on
/// its fake identity authority from another thread. The authority, the
/// Outlook host's redirect and the source's CA reach this process through the
/// three gated seeds (`FAUNA_E2E_MICROSOFT_AUTHORITY`, `FAUNA_E2E_IMAP_HOST_MAP`,
/// `FAUNA_E2E_IMAP_EXTRA_CA_PEM`), which the caller sets in its environment
/// first (`fauna_ffi.py`'s `harness_mail_import_outlook` does).
///
/// `out` receives the reply [`fauna_harness_mail_import`] does; a sign-in that
/// ended without a grant (declined, expired, refused) is an `ok` reply naming
/// the Source step with its sentence as the error.
///
/// [`microsoft_sign_in_actions`]: fauna_client_mail_settings::microsoft_sign_in_actions
///
/// # Safety
///
/// `nest_url` and `username` must be valid NUL-terminated C strings; `secret`
/// must point to `secret_len` readable bytes; `out` must be a valid, aligned,
/// writable pointer to an `FfiBuffer`. All pointers must stay valid for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_mail_import_outlook(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    username: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_mail_settings::{
        DEFAULT_MAX_SIZE_MB, ImportSourceKind, ImportStep, MailImportAction, MicrosoftSignInState,
        microsoft_sign_in_actions, scope_next_actions,
    };

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let (call, username) = unsafe {
            (
                decode_call(nest_url, secret, secret_len, std::ptr::null(), 0)?,
                cstr_to_string(username)?,
            )
        };
        with_client(call.nest_url, call.identity, |nest| async move {
            let machine = fauna_client_mail_settings::rpc_glue::build_mail_import_machine(nest);
            // As in the Generic walk, a refusal lands on the snapshot and the
            // walk stops there.
            let walk = async {
                let source = [
                    MailImportAction::Refresh,
                    MailImportAction::SelectSourceKind {
                        kind: ImportSourceKind::Outlook,
                    },
                ];
                for action in source
                    .into_iter()
                    .chain(microsoft_sign_in_actions(&username))
                {
                    machine.dispatch(action).await.ok()?;
                }
                let waiting = machine
                    .snapshot()
                    .microsoft_signin
                    .is_some_and(|s| s.state == MicrosoftSignInState::Waiting);
                if !waiting {
                    return None;
                }
                machine.run_microsoft_sign_in().await.ok()?;
                if machine.snapshot().step != ImportStep::Scope {
                    return None;
                }
                for action in scope_next_actions("", DEFAULT_MAX_SIZE_MB) {
                    machine.dispatch(action).await.ok()?;
                }
                machine.dispatch(MailImportAction::Start).await.ok()?;
                if machine.snapshot().step == ImportStep::Progress {
                    let _ = machine.run_import().await;
                }
                Some(())
            };
            walk.await;
            let snap = machine.snapshot();
            let reply = (
                format!("{:?}", snap.step),
                snap.imported_count,
                snap.skipped_count,
                snap.errored_count,
                snap.error.unwrap_or_default(),
                snap.error_log,
            );
            fauna_core::encoding::canonical_encode(&reply)
                .map(Outcome::Ok)
                .map_err(encode_err)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// One whole archive import as an app-less client: the shared
/// [`fauna_archive_import_machine::ArchiveImportMachine`], built by the same
/// `rpc_glue::build_archive_import_machine` tui builds it with, hydrated, then
/// walked Source (Facebook) → Archive (`zip_path` opened and indexed) → Scope
/// (the defaults: every importable category, original audiences, no date
/// bounds) → Confirm → Start, with its run loop driven to the end. It is a
/// witness's door to the claims `archive-import.md` makes of the import CLIENT
/// rather than of any app's UI — *the app parses; the nest stores sealed*
/// (§ Architectural rules) and the origin a backdated post carries (§ What each
/// category becomes) — so nothing here re-derives a request.
///
/// The machine's custody, period-key and mail doors are a [`Seat`]'s for the
/// length of the call (module docs, *How an app-less process reaches custody*),
/// and the folder's files are stamped with the seat's device id.
///
/// `out` receives `{"ok": [step, imported, skipped, errored, error, skip_log,
/// folder_name]}` — the machine's final snapshot: the wizard step it ended on
/// (`"Done"` for a finished import), its three counters, the error text (empty
/// for none), the skip log and the archive folder's name (empty before Start).
/// A walk that stopped early is an `ok` reply naming the step it stopped on; a
/// non-zero return is a local failure only.
///
/// # Safety
///
/// `nest_url` and `zip_path` must be valid NUL-terminated C strings; `secret`
/// must point to `secret_len` readable bytes; `out` must be a valid, aligned,
/// writable pointer to an `FfiBuffer`. All pointers must stay valid for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_archive_import(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    zip_path: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_archive_import_machine::{ArchiveImportAction, ArchiveImportStep, ArchiveSourceKind};

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let (call, zip_path) = unsafe {
            (
                decode_call(nest_url, secret, secret_len, std::ptr::null(), 0)?,
                cstr_to_string(zip_path)?,
            )
        };
        let identity = call.identity;
        let device_id = fauna_core::hex32::decode(&seat_device_id(&identity))
            .map_err(|e| err(format!("the seat's device id: {e}")))?;
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            let seat = Seat::start(&nest, &identity).await?;
            let handle = seat.handle.clone();
            let machine = fauna_archive_import_machine::rpc_glue::build_archive_import_machine(
                Arc::clone(&nest),
                device_id,
                Arc::new(handle.clone()),
                Arc::new(seat.custody()),
                Arc::new(handle),
            )
            .map_err(|e| err(format!("build the archive-import machine: {e}")))?;
            // Each step's refusal lands on the snapshot (`error`, the step it
            // stayed on), which is what the reply reports — so the walk stops
            // at the first refused dispatch.
            let walk = async {
                machine.hydrate().await.ok()?;
                let steps = [
                    ArchiveImportAction::SelectSource {
                        kind: ArchiveSourceKind::Facebook,
                    },
                    ArchiveImportAction::Next,
                    ArchiveImportAction::SetArchivePath { value: zip_path },
                    ArchiveImportAction::OpenArchive,
                    ArchiveImportAction::Next,
                    ArchiveImportAction::Next,
                    ArchiveImportAction::Start,
                ];
                for action in steps {
                    machine.dispatch(action).await.ok()?;
                }
                if machine.snapshot().step == ArchiveImportStep::Progress {
                    let _ = machine.run_import().await;
                }
                Some(())
            };
            walk.await;
            let snap = machine.snapshot();
            seat.stop().await;
            let reply = (
                format!("{:?}", snap.step),
                snap.imported,
                snap.skipped,
                snap.errored,
                snap.error.unwrap_or_default(),
                snap.skip_log,
                snap.folder_name.unwrap_or_default(),
            );
            fauna_core::encoding::canonical_encode(&reply)
                .map(Outcome::Ok)
                .map_err(encode_err)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// The group-crypto seam of a harness [`fauna_client_folders::orchestration::FoldersAuthor`]
/// that serves only **unshared** sets ([`fauna_harness_serve_set`]): an
/// unshared set's serve flips touch custody and the nest's flag, never an MLS
/// group, so every group operation is refused rather than faked. A shared set
/// is served through an app, whose conversations rail holds the group.
struct UnsharedSetsOnly;

impl UnsharedSetsOnly {
    fn refused() -> String {
        "the harness serves only unshared sets — a shared set's group lives in an app".into()
    }
}

impl fauna_client_folders::orchestration::FolderGroupCrypto for UnsharedSetsOnly {
    type Error = String;

    fn create_group(
        &self,
        _member_key_packages: &[Vec<u8>],
    ) -> Result<fauna_client_folders::orchestration::CreatedGroup, String> {
        Err(Self::refused())
    }
    fn holds_group(&self, _channel_id: &[u8; 32]) -> Result<bool, String> {
        Ok(false)
    }
    fn channel_id_for_group(&self, raw_group_id: &[u8]) -> [u8; 32] {
        fauna_mls::types::ChannelId::from_group_id(raw_group_id).0
    }
    fn add_member_staged(
        &self,
        _channel_id: &[u8; 32],
        _key_package_bytes: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        Err(Self::refused())
    }
    fn contains_member(
        &self,
        _channel_id: &[u8; 32],
        _member: &fauna_core::identity::ActorId,
    ) -> Result<bool, String> {
        Err(Self::refused())
    }
    fn remove_member_staged(
        &self,
        _channel_id: &[u8; 32],
        _member: &fauna_core::identity::ActorId,
    ) -> Result<Option<Vec<u8>>, String> {
        Err(Self::refused())
    }
    fn merge_pending_commit(&self, _channel_id: &[u8; 32]) -> Result<(), String> {
        Err(Self::refused())
    }
    fn has_pending_commit(&self, _channel_id: &[u8; 32]) -> Result<bool, String> {
        Ok(false)
    }
    fn pending_commit_hash(&self, _channel_id: &[u8; 32]) -> Result<Option<[u8; 32]>, String> {
        Ok(None)
    }
    fn clear_pending_commit(&self, _channel_id: &[u8; 32]) -> Result<(), String> {
        Ok(())
    }
    fn persist_group_state(&self, _channel_id: &[u8; 32]) -> Result<(), String> {
        Ok(())
    }
    fn seal_envelope(
        &self,
        _channel_id: &[u8; 32],
        _payload: &fauna_core::folder_keys::ContentKeyEnvelopePayload,
    ) -> Result<(Vec<u8>, u64), String> {
        Err(Self::refused())
    }
    fn envelope_epoch(&self, _channel_id: &[u8; 32]) -> Result<u64, String> {
        Err(Self::refused())
    }
    fn open_envelope(
        &self,
        _channel_id: &[u8; 32],
        _sealed: &[u8],
    ) -> Result<fauna_core::folder_keys::ContentKeyEnvelopePayload, String> {
        Err(Self::refused())
    }
}

/// Serve, or unserve, one of the owner's **unshared** sets over WebDAV, as the
/// account whose seed is `secret` — the owner's per-set toggle, app-less: the
/// shared [`fauna_client_folders::orchestration::FoldersAuthor::serve_enable`] (a genesis
/// content key and the serve-on stamp in one custody write, then the nest's
/// `webdav_enabled` flag) or
/// [`fauna_client_folders::orchestration::FoldersAuthor::serve_disable`] (the served era
/// adopted, the flag off, every principal's folder read twin over the set
/// revoked nest first then in the signed log, the key rotated and the
/// serve-off stamped), over a [`Seat`]'s custody and grant log, then one
/// publishing pass. The `WebdavKeysBlob` re-provision a face runs after the
/// flip is NOT run: it seals under the account's MSEK, which an app-less seat
/// does not hold, and nothing a principal reads depends on it — the principal
/// lends its own keys (`webdav-server.md` § Key model → *A principal's read*
/// (3)).
///
/// `request` is the dag-cbor array `[set_name, enable]`. `out` receives
/// `{"ok": true}` or `{"err": RpcError}` (a nest refusal of the flag flip).
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_serve_set(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_folders::orchestration::{FoldersAuthor, FoldersAuthorError};

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (name, enable): (String, bool) = decode_request!(call.req)?;
        let identity = call.identity;
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            let seat = Seat::start(&nest, &identity).await?;
            let author = FoldersAuthor::new(
                FoldersClient::new(Arc::clone(&nest)),
                identity.clone_keypair(),
                Arc::new(seat.custody()),
                Arc::new(seat.handle.clone()),
                UnsharedSetsOnly,
            )
            .with_grant_log(Arc::new(seat.handle.clone()));
            let flipped = if enable {
                author.serve_enable(&name, None).await
            } else {
                author.serve_disable(&name, None).await.map(|_| ())
            };
            let pass_errors = seat.publish().await;
            seat.stop().await;
            let reply: Result<bool, NestClientError> = match flipped {
                Ok(()) => Ok(true),
                Err(FoldersAuthorError::Transport(NestClientError::Rpc(e))) => {
                    Err(NestClientError::Rpc(e))
                }
                Err(e) => return Err(err(format!("serve {name:?} (enable: {enable}): {e}"))),
            };
            pass_errors?;
            outcome!(reply)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Approve one pending consent request **through the production approving
/// path**, as the account whose seed is `secret`: the consent machine's shared
/// [`fauna_atproto_settings_machine::consent_grant::answer_consent`] — the row
/// read off `fauna.bridges.atproto.list_pending_consents`, its grants planned
/// by `prepare_ext_consent_grant` over seams wired exactly as tui's and
/// linux's are (`ConsentGrantSeams::with_nest_custody` over a [`Seat`]'s grant
/// log, kind-manifest rows and folder custody), each deposited
/// (`fauna.capabilities.mint`), then `fauna.bridges.atproto.resolve_consent` —
/// the steps an app's approve tap runs. `folder` is the card's choice for a
/// row carrying a bare folder-plane scope, `null` for none.
///
/// `request` is the dag-cbor array `[consent_id, folder]` (bytes, an integer
/// or null). `out` receives `{"ok": {"resolved": bool}}` — the resolve's own
/// answer — or `{"ok": {"refused": "<reason>"}}` when the approve refused
/// before anything was deposited (the card's error line: an unserved folder,
/// keys this account does not hold). A row the list does not carry, or a nest
/// call that failed, is a local failure.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_answer_consent(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_atproto_settings_machine::consent_grant::{
        AnswerError, ConsentGrantSeams, answer_consent,
    };
    use fauna_atproto_settings_machine::nest_api::AtprotoSettingsNestApi;
    use fauna_atproto_settings_machine::nest_api::ws_rpc::WsRpcAtprotoSettingsNest;
    use std::collections::BTreeMap;

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (consent_id, folder): (fauna_protocol::ByteBuf, Option<i64>) =
            decode_request!(call.req)?;
        let identity = call.identity;
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            let api = WsRpcAtprotoSettingsNest::new(Arc::clone(&nest));
            let row = api
                .list_pending_consents()
                .await
                .map_err(|e| err(format!("list pending consents: {}", e.detail())))?
                .into_iter()
                .find(|r| r.consent_id == consent_id.as_slice())
                .ok_or_else(|| err("no pending consent carries that id"))?;
            let seat = Seat::start(&nest, &identity).await?;
            let seams = ConsentGrantSeams::from_keypair(
                &identity,
                Arc::new(seat.handle.clone()),
                Arc::new(seat.handle.clone()),
            )
            .with_nest_custody(Arc::new(seat.custody()), Arc::clone(&nest));
            let answer = answer_consent(
                Some(&seams),
                &row,
                true,
                folder,
                |blob| api.mint_grant(blob),
                |id, approved, folder| api.resolve_consent(id, approved, folder),
                |grant_id| api.revoke_grant(grant_id),
            )
            .await;
            let pass_errors = seat.publish().await;
            seat.stop().await;
            match answer {
                Ok(resolved) => {
                    pass_errors?;
                    fauna_core::encoding::canonical_encode(&BTreeMap::from([(
                        "resolved", resolved,
                    )]))
                }
                Err(AnswerError::Grant(e)) => fauna_core::encoding::canonical_encode(
                    &BTreeMap::from([("refused", e.to_string())]),
                ),
                Err(AnswerError::Nest(e)) => {
                    return Err(err(format!("answer the consent: {}", e.detail())));
                }
            }
            .map(Outcome::Ok)
            .map_err(encode_err)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// A fresh ATProto **user rotation key** — the client-held senior key a hosted
/// mint publishes as `rotationKeys[0]` — generated through the shared
/// [`fauna_client_atproto::rotation_key::generate_rotation_key`], the keygen
/// every app's mint path runs. `out` receives `{"secret": <64 hex>,
/// "did_key": "did:key:zDn…"}`: the test HOLDS the key, so it can later sign
/// for the identity it published, as the user's client does. Not networked.
///
/// # Safety
///
/// `out` must be a valid, aligned, writable pointer to an `FfiBuffer`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_atproto_rotation_key(out: *mut FfiBuffer) -> i32 {
    let body = || -> Result<Vec<u8>, FfiError> {
        let key = fauna_client_atproto::rotation_key::generate_rotation_key(0);
        serde_json::to_vec(&serde_json::json!({
            "secret": hex::encode(key.secret_scalar.to_array()),
            "did_key": key.pubkey_did_key,
        }))
        .map_err(|e| err(format!("encode the key: {e}")))
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { write_out(body(), out) }
}

/// Permanently retire `did` — publish its PLC tombstone at `directory_url`,
/// signed with the held rotation key (`secret`, published as `did_key`) —
/// through the shared steps of
/// [`fauna_client_atproto::tombstone::converge_retirement`]: ONE read of the
/// directory's standing head, the already-retired short-circuit
/// ([`retirement_step`](fauna_client_atproto::tombstone::retirement_step)), the
/// signer the head lists ([`select_signer`](fauna_client_atproto::tombstone::select_signer)),
/// then [`sign_tombstone`](fauna_client_atproto::tombstone::sign_tombstone) and
/// [`submit_tombstone`](fauna_client_atproto::tombstone::submit_tombstone).
///
/// The one step it leaves to the caller is `converge_retirement`'s sweep probe:
/// that asks the PDS the DID's log *publishes* (`https://pds.<domain>`), a
/// name an app-less harness cannot reach, so a witness first observes the
/// sweep finished on the bridge's own XRPC listener and then calls this. The
/// probe is an ordering politeness, not a boundary (its own doc says so); the
/// signed bytes and the submit are the apps' own.
///
/// `out` receives `{"prev_cid": <the head it chained to>}` — what
/// `fauna.bridges.atproto.record_tombstone` reports — or `{"prev_cid": ""}`
/// when the log was already retired (the report then chains nothing, as that
/// kind's own doc describes). A refused submit is a local failure carrying the
/// directory's reason.
///
/// # Safety
///
/// `directory_url`, `did` and `did_key` must be valid NUL-terminated C
/// strings; `secret` must point to 32 readable bytes; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`. All must stay valid for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_atproto_retire(
    directory_url: *const c_char,
    did: *const c_char,
    secret: *const u8,
    did_key: *const c_char,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_atproto::genesis_verify::fetch_audit_log;
    use fauna_client_atproto::tombstone::{
        StepPlan, retirement_step, select_signer, sign_tombstone, standing_head, submit_tombstone,
    };

    let body = || -> Result<Vec<u8>, FfiError> {
        // SAFETY: per the `# Safety` section above.
        let (directory_url, did, did_key) = unsafe {
            (
                cstr_to_string(directory_url)?,
                cstr_to_string(did)?,
                cstr_to_string(did_key)?,
            )
        };
        // SAFETY: per the `# Safety` section above.
        let secret = unsafe { super::read_32(secret, "secret")? };
        let held = [fauna_core::data::AtprotoRotationKey {
            secret_scalar: secret.into(),
            pubkey_did_key: did_key,
            created_at: 0,
            published_for_dids: Vec::new(),
        }];
        let prev_cid = runtime()?.block_on(async {
            let log = fetch_audit_log(&directory_url, &did)
                .await
                .map_err(|e| err(format!("read the audit log: {e}")))?;
            let head = standing_head(&log).map_err(|e| err(format!("standing head: {e}")))?;
            if let StepPlan::AlreadyRetired =
                retirement_step(&head).map_err(|e| err(format!("retirement step: {e}")))?
            {
                return Ok::<_, FfiError>(String::new());
            }
            let signer =
                select_signer(&held, &head).map_err(|e| err(format!("select signer: {e}")))?;
            let op = sign_tombstone(&head.cid, &signer.secret_scalar.to_array())
                .map_err(|e| err(format!("sign the tombstone: {e}")))?;
            submit_tombstone(&directory_url, &did, &op)
                .await
                .map_err(|e| err(format!("submit the tombstone: {e}")))?;
            Ok(head.cid)
        })?;
        serde_json::to_vec(&serde_json::json!({ "prev_cid": prev_cid }))
            .map_err(|e| err(format!("encode: {e}")))
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { write_out(body(), out) }
}

/// Mint a trained topic named `name` as the actor `secret` names, through the
/// shared [`fauna_sync_engine::preference_surfaces::create_trained_topic`]
/// over a [`Seat`]'s account store — the gesture every app's *New topic* runs
/// (`topic-factors.md` § Authoring surface & picker) — then one publishing
/// pass, so the registry entry rests on the nest as the sealed
/// `fauna.state.personalization` row and nowhere else (§ At rest — seal +
/// home).
///
/// The proof the next reader depends on: the registry read back off the nest
/// through a SECOND seat, over a fresh store whose prologue walks only what
/// the account holds nest-side. A name that stayed in the first seat's store
/// fails the call. Each seat is a fleet enrollment, so the account gains two
/// per call (and one `sync_devices` row per account, as
/// [`fauna_harness_create_set`]).
///
/// `request` is the dag-cbor array `[name]`. `out` receives `{"ok": <the
/// factor's composition key, "topic:<hex>">}`.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_topic_create(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_personalization::TrainedTopics;
    use fauna_sync_engine::preference_surfaces;

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (name,): (String,) = decode_request!(call.req)?;
        let reader = (call.nest_url.clone(), call.identity.clone_keypair());
        let (key, pass_errors) = with_seat(
            call.nest_url,
            call.identity,
            async |nest, _identity, seat| {
                let svc = TrainedTopics::new(Arc::clone(nest));
                let rows = preference_surfaces::create_trained_topic(&seat.handle, &svc, &name)
                    .await
                    .map_err(|e| err(format!("create the trained topic: {e}")))?;
                // The registry appends, so a repeated name's fresh entry is the last.
                let key = rows
                    .iter()
                    .rev()
                    .find(|r| r.name == name)
                    .and_then(|r| r.factor_key.clone())
                    .ok_or_else(|| err("the created topic is not among the returned rows"))?;
                Ok((key, seat.publish().await?))
            },
        )?;
        let registry = with_seat(reader.0, reader.1, async |_nest, _identity, seat| {
            preference_surfaces::load_personalization(&seat.handle)
                .await
                .map_err(|e| err(format!("read the registry back off the nest: {e:#}")))
        })?;
        if !registry
            .trained_factors
            .iter()
            .any(|m| m.name == name && m.factor_key().as_deref() == Some(key.as_str()))
        {
            return Err(err(format!(
                "the topic {key} was created, but its registry entry did not reach the nest — \
                 the seat's publishing pass reported: {pass_errors:?}"
            )));
        }
        fauna_core::encoding::canonical_encode(&key)
            .map(Outcome::Ok)
            .map_err(encode_err)
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Mark a post **more / less like this** for a trained topic as the actor
/// `secret` names, through the shared [`fauna_feed::FeedManager::train_post`]
/// — the very gesture behind every app's `feed-post-more-like-this` /
/// `feed-post-less-like-this` (`topic-factors.md` § Training signals): the
/// post's full text fetched, the model fetched and unsealed (absent ⇒ fresh),
/// trained, re-sealed under the actor's personalization keys and put. A
/// manager built for the call, with no feed loaded: the train path reads none.
///
/// `request` is the dag-cbor array `[factor_key, post_id_hex, verb]`, `verb`
/// `"more-like-this"` or `"less-like-this"`. `out` receives `{"ok":
/// "trained" | "flipped" | "duplicate-signal"}`. Every failure — a nest
/// refusal included, which the manager reports as text — is local.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_topic_train(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_feed::{FeedManager, TrainResult, TrainVerb};

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (factor, post_id, verb): (String, String, String) = decode_request!(call.req)?;
        let verb = match verb.as_str() {
            "more-like-this" => TrainVerb::MoreLikeThis,
            "less-like-this" => TrainVerb::LessLikeThis,
            other => return Err(err(format!("unknown train verb {other:?}"))),
        };
        let seed = *call.identity.secret_bytes();
        with_client(call.nest_url, call.identity, |nest| async move {
            let result = FeedManager::new(nest, seed)
                .train_post(post_id, factor, verb)
                .await
                .map_err(|e| err(format!("train the post: {e}")))?;
            let result = match result {
                TrainResult::Trained => "trained",
                TrainResult::Flipped => "flipped",
                TrainResult::DuplicateSignal => "duplicate-signal",
            };
            fauna_core::encoding::canonical_encode(&result)
                .map(Outcome::Ok)
                .map_err(encode_err)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Record engagement verdicts and flush them as the actor `secret` names,
/// through the [`fauna_feed::FeedManager`]'s own cue path
/// (`engagement-cues.md` § At rest): [`hydrate_cues`] (the nest's `cues:v1`
/// rollup fetched and opened, absent ⇒ empty), one [`record_observation`] per
/// item, then [`flush_cues`] — sealed under the actor's personalization keys
/// and put under `cues:v1`.
///
/// **The one staged part is each observation.** An app's
/// [`fauna_feed::CueTracker`] assembles them from its geometry probe; this
/// export builds the exposure that derives the asked-for verdict under the
/// engine's own thresholds (`fauna_core::scoring::cues`) — a non-media item
/// held at long visibility for [`CUE_DWELL_LONG_MS`] for `watch-complete`, one
/// glimpsed for half of [`CUE_SKIP_MS`] and scrolled past for `skip` — spaced
/// past the burst gate, and fails unless the engine derives exactly that
/// verdict. Derivation, fold, seal and put are all production.
///
/// [`hydrate_cues`]: fauna_feed::FeedManager::hydrate_cues
/// [`record_observation`]: fauna_feed::FeedManager::record_observation
/// [`flush_cues`]: fauna_feed::FeedManager::flush_cues
/// [`CUE_DWELL_LONG_MS`]: fauna_core::scoring::cues::CUE_DWELL_LONG_MS
/// [`CUE_SKIP_MS`]: fauna_core::scoring::cues::CUE_SKIP_MS
///
/// `request` is the dag-cbor array `[[content_id, verdict], …]`, `verdict`
/// `"watch-complete"` or `"skip"`. `out` receives `{"ok": <items recorded>}`.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_cues_record_and_flush(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_core::scoring::cues::{CUE_BURST_MIN_GAP_MS, CUE_DWELL_LONG_MS, CUE_SKIP_MS};
    use fauna_feed::{CueObservation, ExposureEnd, FeedManager};

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let items: Vec<(String, String)> = decode_request!(call.req)?;
        let seed = *call.identity.secret_bytes();
        with_client(call.nest_url, call.identity, |nest| async move {
            let feed = FeedManager::new(nest, seed);
            feed.hydrate_cues()
                .await
                .map_err(|e| err(format!("hydrate the cue rollup: {e}")))?;
            let start_ms = fauna_core::data::Timestamp::now().0 / 1_000;
            let gap_ms = CUE_DWELL_LONG_MS + CUE_BURST_MIN_GAP_MS;
            for (i, (content_id, verdict)) in items.iter().enumerate() {
                let (dwell_skip, dwell_long) = match verdict.as_str() {
                    "watch-complete" => (CUE_DWELL_LONG_MS, CUE_DWELL_LONG_MS),
                    "skip" => (CUE_SKIP_MS / 2, 0),
                    other => return Err(err(format!("unknown cue verdict {other:?}"))),
                };
                let derived = feed
                    .record_observation(CueObservation {
                        content_id: content_id.clone(),
                        is_media: false,
                        media_played_pm: None,
                        dwell_ms_at_skip_visibility: dwell_skip,
                        dwell_ms_at_long_visibility: dwell_long,
                        observed_at_ms: start_ms + (i as u64 + 1) * gap_ms,
                        ended_by: ExposureEnd::ScrolledPast,
                    })
                    .await
                    .map_err(|e| err(format!("record the observation: {e}")))?;
                if derived.as_ref().and_then(|v| v.wire_str()) != Some(verdict.as_str()) {
                    return Err(err(format!(
                        "the staged exposure of {content_id:?} derived {derived:?}, not {verdict:?}"
                    )));
                }
            }
            feed.flush_cues()
                .await
                .map_err(|e| err(format!("flush the cue rollup: {e}")))?;
            fauna_core::encoding::canonical_encode(&(items.len() as u64))
                .map(Outcome::Ok)
                .map_err(encode_err)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Turn a trained topic's **learn from my activity** opt-in on or off as the
/// actor `secret` names, through the shared
/// [`fauna_sync_engine::preference_surfaces::set_trained_topic_engagement`]
/// over a [`Seat`]'s account store — the toggle behind every app's topic row
/// (`topic-factors.md` § Training signals) — then one publishing pass. As
/// [`fauna_harness_topic_create`], the flag is read back off the nest through
/// a second, fresh seat, and one that stayed in the first seat's store fails
/// the call.
///
/// `request` is the dag-cbor array `[factor_key, on]`. `out` receives `{"ok":
/// on}`.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_topic_set_engagement(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_personalization::TrainedTopics;
    use fauna_sync_engine::preference_surfaces;

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (factor, on): (String, bool) = decode_request!(call.req)?;
        let id = fauna_core::scoring::topic_factor_id(&factor)
            .ok_or_else(|| err(format!("not a topic composition key: {factor:?}")))?;
        let reader = (call.nest_url.clone(), call.identity.clone_keypair());
        let pass_errors = with_seat(
            call.nest_url,
            call.identity,
            async |nest, _identity, seat| {
                let svc = TrainedTopics::new(Arc::clone(nest));
                preference_surfaces::set_trained_topic_engagement(&seat.handle, &svc, &id, on)
                    .await
                    .map_err(|e| err(format!("set the topic's engagement opt-in: {e}")))?;
                seat.publish().await
            },
        )?;
        let registry = with_seat(reader.0, reader.1, async |_nest, _identity, seat| {
            preference_surfaces::load_personalization(&seat.handle)
                .await
                .map_err(|e| err(format!("read the registry back off the nest: {e:#}")))
        })?;
        if !registry.trained_factors.iter().any(|m| {
            m.factor_key().as_deref() == Some(factor.as_str()) && m.learn_from_engagement == on
        }) {
            return Err(err(format!(
                "the opt-in of {factor} was set, but the registry on the nest does not read \
                 {on} — the seat's publishing pass reported: {pass_errors:?}"
            )));
        }
        fauna_core::encoding::canonical_encode(&on)
            .map(Outcome::Ok)
            .map_err(encode_err)
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Teach a trained topic one **engagement** example as the actor `secret`
/// names: the post's verdict moving from none to `verdict` (`watch-complete` ⇒
/// a weak *more like this*, `skip` ⇒ a weak *less like this*) trains the
/// topic's model through the production [`fauna_feed::TopicModel::train_engagement`],
/// which is then re-sealed under the actor's personalization keys and put —
/// Layer A of `engagement-cues.md`, applied to the model at rest.
///
/// **Why not the cue path.** [`fauna_harness_cues_record_and_flush`] runs the
/// manager's own observe → fold → flush, but the manager trains engagement
/// only for the factors its *loaded feed* composes, and only from that feed's
/// window text (`FeedManager::train_engagement_for_transition`); a manager
/// built for a headless call has loaded no feed, so that path folds the
/// verdict into the rollup and teaches no model. This export applies the same
/// transition to the model directly instead, so a test can make a topic hold
/// engagement-taught counts — what the publish witness needs, since its point
/// is that the published artifact ignores whatever engagement taught, however
/// it was taught.
///
/// What it keeps of production: the opt-in gate — a topic whose registry entry
/// (read off the nest through a seat) has *learn from my activity* off is
/// refused, as the manager composes no engagement for it — the model text's
/// shape ([`fauna_feed::model_text`] with no title, the shape the window
/// trains with; the post's body here, which for a post under the preview
/// length is the same text), and a model that does not exist yet is refused
/// rather than created (the manager trains only a loaded model).
///
/// `request` is the dag-cbor array `[factor_key, post_id_hex, verdict]`. `out`
/// receives `{"ok": <the model's engagement document count after>}`.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_topic_train_engagement(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_core::post::decode_post;
    use fauna_client_personalization::{
        PersonalizationClient, model_seal_keys, seal_topic_model, unseal_topic_model,
    };
    use fauna_client_posts::PostsClient;
    use fauna_core::crypto::BackupKey;
    use fauna_feed::{TrainVerb, model_text};
    use fauna_sync_engine::preference_surfaces;

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (factor, post_id, verdict): (String, String, String) = decode_request!(call.req)?;
        let label = match verdict.as_str() {
            "watch-complete" => TrainVerb::MoreLikeThis.into(),
            "skip" => TrainVerb::LessLikeThis.into(),
            other => return Err(err(format!("unknown cue verdict {other:?}"))),
        };
        let keys = model_seal_keys(&BackupKey::derive(call.identity.secret_bytes()));
        let count = with_seat(
            call.nest_url,
            call.identity,
            async |nest, _identity, seat| {
                let registry = preference_surfaces::load_personalization(&seat.handle)
                    .await
                    .map_err(|e| err(format!("read the registry: {e:#}")))?;
                let opted_in = registry.trained_factors.iter().any(|m| {
                    m.factor_key().as_deref() == Some(factor.as_str()) && m.learn_from_engagement
                });
                if !opted_in {
                    return Err(err(format!(
                        "the topic {factor} does not learn from activity — no engagement \
                         trains it"
                    )));
                }
                let reply = PostsClient::new(Arc::clone(nest))
                    .posts_get(post_id.clone())
                    .await
                    .map_err(|e| err(format!("fetch the post: {e}")))?;
                let (post, _origin) = decode_post(reply.body.as_ref())
                    .map_err(|e| err(format!("decode the post: {e}")))?;
                let text = model_text(None, &post.body_text(), &post.tags());

                let models = PersonalizationClient::new(Arc::clone(nest));
                let blob = models
                    .model_fetch(factor.clone())
                    .await
                    .map_err(|e| err(format!("fetch the model: {e}")))?
                    .sealed_blob
                    .ok_or_else(|| err(format!("the topic {factor} has no model to train")))?;
                let mut model = unseal_topic_model(&blob, &keys)
                    .map_err(|e| err(format!("open the model: {e}")))?;
                model.train_engagement(&text, None, Some(label));
                let sealed =
                    seal_topic_model(&model, &keys).map_err(|e| err(format!("seal: {e}")))?;
                models
                    .model_put(factor.clone(), sealed, model.example_count())
                    .await
                    .map_err(|e| err(format!("put the model: {e}")))?;
                Ok(model.engagement_example_count())
            },
        )?;
        fauna_core::encoding::canonical_encode(&count)
            .map(Outcome::Ok)
            .map_err(encode_err)
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Publish a trained topic as a tier-3 `text-model` labeler named
/// `public_name`, as the actor `secret` names — the app's whole publish
/// gesture (`topic-factors.md` § Publishing a trained factor, v2): the
/// rebuild through the shared [`fauna_feed::FeedManager::scrub_corpus_for_factor`]
/// (the factor's markers walked, each marked post fetched, every one not
/// publicly readable dropped, the rest re-tokenized and scrubbed), then the
/// shared [`fauna_client_personalization::publish::publish_trained_factor_model`]
/// with the review's whole n-gram set — what the apps'
/// `trained_topic_publish_model` does when the user leaves every row of the
/// sheet checked. The private model is a parameter of neither, as there.
///
/// `request` is the dag-cbor array `[factor_key, public_name]`. `out` receives
/// `{"ok": {"labeler_id": <hex>, "version": u64, "ngram_count": u64,
/// "more_docs": u32, "less_docs": u32, "included_examples": u32,
/// "marked_examples": u32}}` — the artifact itself is the nest's to show
/// (`fauna.labelers.inspect`).
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_topic_publish_model(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_personalization::publish::publish_trained_factor_model;
    use fauna_feed::FeedManager;

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (factor, public_name): (String, String) = decode_request!(call.req)?;
        let id = fauna_core::scoring::topic_factor_id(&factor)
            .ok_or_else(|| err(format!("not a topic composition key: {factor:?}")))?;
        let seed = *call.identity.secret_bytes();
        with_client(call.nest_url, call.identity, |nest| async move {
            let review = FeedManager::new(Arc::clone(&nest), seed)
                .scrub_corpus_for_factor(&factor)
                .await
                .map_err(|e| err(format!("rebuild the factor's vocabulary: {e}")))?;
            let published = publish_trained_factor_model(
                nest,
                &seed,
                &id,
                &public_name,
                review.more_docs,
                review.less_docs,
                review
                    .ngrams
                    .into_iter()
                    .map(|n| (n.ngram, n.more, n.less))
                    .collect(),
            )
            .await
            .map_err(|e| err(format!("publish the model: {e}")))?;
            fauna_core::encoding::canonical_encode(&serde_json::json!({
                "labeler_id": hex::encode(published.labeler_id),
                "version": published.version,
                "ngram_count": published.ngram_count as u64,
                "more_docs": review.more_docs,
                "less_docs": review.less_docs,
                "included_examples": review.included_examples,
                "marked_examples": review.marked_examples,
            }))
            .map(Outcome::Ok)
            .map_err(encode_err)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// The device row a peer seat labelled `label` enrolls on — one per (account,
/// label), so two peer seats of one account are two devices of it, each with
/// its own enrolled principal (a second seat on [`seat_device_id`]'s row
/// would replace the first's grant, and the relay would then refuse the
/// first seat's key).
fn peer_seat_device_id(identity: &ActorKeypair, label: &str) -> String {
    let mut material = identity.actor_id().0.to_vec();
    material.extend_from_slice(label.as_bytes());
    hex::encode(blake3::derive_key(
        "fauna e2e harness peer seat device id v1",
        &material,
    ))
}

/// The pause between a peer seat's passes. Pacing only — what a test waits
/// for is read off the reports the passes write, never off this.
const PEER_SEAT_PASS_PACE: std::time::Duration = std::time::Duration::from_millis(500);

/// What a peer seat has seen across its passes — written after every pass to
/// the report file and returned when the seat stops. Counts are maxima over
/// passes (a pass before the sibling was up counts nothing; a later one that
/// admitted it is what a test asserts on), `applied` a sum.
#[derive(Default)]
struct PeerSeatTally {
    passes: u64,
    /// Whether the last pass was the engine holder's — the peer leg is a
    /// holder's step, so a seat that never holds never binds.
    holder: bool,
    /// The last pass's enrollment outcome — a seat the nest would not
    /// enroll never binds either.
    enrollment: Option<String>,
    bound: bool,
    peer_leg: Option<String>,
    targets: usize,
    admitted: usize,
    admitted_relayed: usize,
    applied: usize,
    blocks_relay_deferred: usize,
    blocks_fetched: usize,
    failed: usize,
    errors: Vec<String>,
}

impl PeerSeatTally {
    fn fold(&mut self, report: &fauna_sync_engine::account_runtime::PumpReport) {
        use fauna_sync_engine::account_runtime::PeerLegPass;
        self.passes += 1;
        self.holder = !report.skipped_non_holder;
        if let Some(enrollment) = &report.enrollment {
            self.enrollment = Some(format!("{enrollment:?}"));
        }
        if let Some(leg) = &report.peer_leg {
            self.bound |= matches!(leg, PeerLegPass::Bound | PeerLegPass::AlreadyBound);
            self.peer_leg = Some(format!("{leg:?}"));
        }
        if let Some(dial) = &report.peer_dial {
            self.targets = self.targets.max(dial.targets);
            self.admitted = self.admitted.max(dial.admitted);
            self.admitted_relayed = self.admitted_relayed.max(dial.admitted_relayed);
            self.applied += dial.applied;
            self.blocks_relay_deferred = self.blocks_relay_deferred.max(dial.blocks_relay_deferred);
            self.blocks_fetched += dial.blocks_fetched;
            self.failed += dial.failed;
        }
        self.errors = report.errors.clone();
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "passes": self.passes,
            "holder": self.holder,
            "enrollment": self.enrollment,
            "bound": self.bound,
            "peer_leg": self.peer_leg,
            "targets": self.targets,
            "admitted": self.admitted,
            "admitted_relayed": self.admitted_relayed,
            "applied": self.applied,
            "blocks_relay_deferred": self.blocks_relay_deferred,
            "blocks_fetched": self.blocks_fetched,
            "failed": self.failed,
            "errors": self.errors,
        })
    }
}

/// Write `value` to `path` whole — a temp file renamed over it, so a reader
/// polling the path never sees half a report.
fn write_report(path: &std::path::Path, value: &serde_json::Value) -> Result<(), FfiError> {
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec(value).map_err(|e| err(format!("encode the report: {e}")))?;
    std::fs::write(&tmp, bytes).map_err(|e| err(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| err(format!("publish {}: {e}", path.display())))
}

/// Host one **peer seat** of the account `secret` names — the production
/// runtime as a seed-holding principal on a device row of its own
/// ([`peer_seat_device_id`] of `seat_label`), its peer leg bound through
/// [`fauna_iroh::peer_leg_transport_trusting`]: the factory body every native
/// app hands the runtime, trusting `relay_root` (one DER certificate) for the
/// relay's TLS in place of the platform roots — the one thing a docker nest
/// under a test domain cannot get a public CA to issue. The relay URL is the
/// nest's own advertisement, read by the runtime as in an app.
///
/// It runs passes until `release_path` exists or `budget_secs` runs out,
/// writing after every pass a JSON report to `report_path` — whether the leg
/// bound, and over its dial passes the most sibling targets, admitted
/// siblings, siblings admitted over a relay, and wanted blocks left to the
/// nest because the connection was relayed (`p2p.md` § The relay, ruling 4)
/// — so a test reads two seats' progress while both run. `out` receives
/// `{"ok": <the last report>}`.
///
/// What it leaves on the account is what a device signing in leaves: the
/// device row, its enrolled principal, its endpoints entry.
///
/// # Safety
///
/// `nest_url`, `seat_label`, `report_path` and `release_path` must be valid
/// NUL-terminated C strings; `secret` / `relay_root` must point to
/// `secret_len` / `relay_root_len` readable bytes; `out` must be a valid,
/// aligned, writable pointer to an `FfiBuffer`. All pointers must stay valid
/// for the duration of the call.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)] // a C ABI: one argument per fact
pub unsafe extern "C" fn fauna_harness_peer_seat_hold(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    seat_label: *const c_char,
    relay_root: *const u8,
    relay_root_len: u32,
    report_path: *const c_char,
    release_path: *const c_char,
    budget_secs: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let (call, label, root, report_path, release_path) = unsafe {
            (
                decode_call(nest_url, secret, secret_len, std::ptr::null(), 0)?,
                cstr_to_string(seat_label)?,
                slice_to_vec(relay_root, relay_root_len),
                std::path::PathBuf::from(cstr_to_string(report_path)?),
                std::path::PathBuf::from(cstr_to_string(release_path)?),
            )
        };
        if root.is_empty() {
            return Err(err("relay_root must be one DER certificate"));
        }
        let identity = call.identity.clone_keypair();
        let budget = std::time::Duration::from_secs(u64::from(budget_secs));
        with_client(call.nest_url, call.identity, |nest| async move {
            let factory: PeerTransportFactory = Arc::new(move |inputs: PeerLegFactoryInputs| {
                let root = root.clone();
                Box::pin(async move {
                    let (transport, bound_addrs) = fauna_iroh::peer_leg_transport_trusting(
                        inputs.writer_key.to_bytes(),
                        inputs.relay_url.as_deref(),
                        vec![root],
                    )
                    .await?;
                    Ok(PeerLegBinding {
                        transport,
                        bound_addrs,
                        file_sync: None,
                    })
                })
            });
            let device_id = peer_seat_device_id(&identity, &label);
            let seat = Seat::start_on(&nest, &identity, device_id, Some(factory)).await?;
            let deadline = tokio::time::Instant::now() + budget;
            let mut tally = PeerSeatTally::default();
            let result = loop {
                match seat.handle.reconcile_now().await {
                    Ok(report) => tally.fold(&report),
                    Err(e) => tally.errors = vec![format!("pass: {e:#}")],
                }
                let value = tally.json();
                if let Err(e) = write_report(&report_path, &value) {
                    break Err(e);
                }
                if release_path.exists() || tokio::time::Instant::now() >= deadline {
                    break Ok(value);
                }
                tokio::time::sleep(PEER_SEAT_PASS_PACE).await;
            };
            seat.stop().await;
            fauna_core::encoding::canonical_encode(&result?)
                .map(Outcome::Ok)
                .map_err(encode_err)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Drive the admin DNS page's machine as an app-less client: the shared
/// [`fauna_client_dns::DnsManagementMachine`], built by the same
/// `build_dns_management_machine_with_credentials` every native app builds it
/// with, hydrated as a page open hydrates it, then `actions` dispatched in
/// order. It is a witness's door to the claims `tls-certificates.md` makes of
/// the issuing CLIENT rather than of any app's UI — § B's *a client-issued
/// order covers every name the listener serves* is the first: an `IssueCert`
/// here runs the very `issue_cert` funnel (the nest's `desired_sans` read,
/// `order_san_set`, the DNS-01 order, the seal, `fauna.tls.publish_cert`) the
/// row's button reaches, so nothing re-derives a request.
///
/// The machine's DNS record (`fauna.state.dns`: the held credential, the
/// managed opt-ins, the ACME account) is a [`Seat`]'s for the length of the
/// call, published before the seat closes. The DNS provider and the CA are the
/// e2e ones exactly as an app's: the fake provider under
/// `FAUNA_DNS_PROVIDER_FAKE` and the e2e ACME directory seed, both read by the
/// shared builder from this process's environment, which the caller sets
/// first (`fauna_ffi.py`'s `harness_dns_dispatch` does).
///
/// `request` is a dag-cbor list of `DnsAction`s. An `IssueCert` or
/// `BeginManualIssueCert` whose `target_nest_id` is empty is aimed the way
/// every app's glue aims it — the nest this connection is bound to
/// ([`fauna_client_pair::resolve_this_nest_id`]). `out` receives
/// `{"ok": [dispatched, error]}`: how many actions succeeded, and the first
/// refusal's text (empty when all did). The walk stops at the first refusal; a
/// non-zero return is a local failure only.
///
/// # Safety
///
/// As [`fauna_harness_create_set`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_dns_dispatch(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_dns::DnsAction;

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let actions: Vec<DnsAction> = decode_request!(call.req)?;
        with_seat(
            call.nest_url,
            call.identity,
            async |nest, identity, seat| {
                let handle = seat.handle.clone();
                let machine = fauna_client_dns::build_dns_management_machine_with_credentials(
                    Arc::clone(nest),
                    identity.clone_keypair(),
                    Arc::new(move || Some(handle.clone())),
                );
                // The page's open, then each action in turn; the first refusal
                // ends the walk and is the reply's text.
                let walk = async {
                    if let Err(e) = machine.hydrate().await {
                        return (0, format!("hydrate: {e}"));
                    }
                    let mut dispatched = 0u32;
                    for mut action in actions {
                        if let DnsAction::IssueCert { target_nest_id, .. }
                        | DnsAction::BeginManualIssueCert { target_nest_id, .. } = &mut action
                            && target_nest_id.is_empty()
                        {
                            match fauna_client_pair::resolve_this_nest_id(nest).await {
                                Ok(id) => *target_nest_id = id,
                                Err(e) => {
                                    return (dispatched, format!("resolve the target nest: {e}"));
                                }
                            }
                        }
                        if let Err(e) = machine.dispatch(action).await {
                            return (dispatched, e.to_string());
                        }
                        dispatched += 1;
                    }
                    (dispatched, String::new())
                };
                let reply = walk.await;
                // What the walk wrote to the DNS record reaches the nest, as an
                // app's would; a step failure of the pass is not this call's.
                seat.publish().await?;
                fauna_core::encoding::canonical_encode(&reply)
                    .map(Outcome::Ok)
                    .map_err(encode_err)
            },
        )
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}
