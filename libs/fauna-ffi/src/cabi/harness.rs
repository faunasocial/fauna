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
//! - [`fauna_harness_create_private_link`] —
//!   [`fauna_client_share::ShareClient::create_private_link`]: a fragment-keyed
//!   share link minted, registered, and its URL revealed against the reply.
//!   Its recipient-side twin, [`fauna_harness_share_viewer_open`], is not
//!   networked: the viewer's own open + assemble over bytes the test fetched.
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
    CloudBackupExclusion, DEFAULT_BACKSTOP_INTERVAL, RuntimePrincipal, StoreRoot, fixed_holders,
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
            peer_transport: None,
            enrollment_target_device_id: seat_device_id(identity),
        })
        .await
        .map_err(|e| err(format!("start the seat's account runtime: {e:#}")))?;
        handle.settled().await;
        Ok(Self { handle, _dir: dir })
    }

    /// The seat's custody door — the one every runtime-hosting app hands the
    /// custody writers.
    fn custody(&self) -> PlaneFolderKeys<impl Fn() -> Option<AccountStoreHandle> + Send + Sync> {
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
    let reply: Result<FolderCreateReply, NestClientError> =
        match fauna_client_folders::create_set(&files, &seat.custody(), req).await {
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
        let identity = call.identity;
        with_client(call.nest_url, identity.clone_keypair(), |nest| async move {
            let seat = Seat::start(&nest, &identity).await?;
            let outcome = create_set_on(&seat, &nest, &identity, req).await;
            seat.stop().await;
            outcome
        })
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
    match body() {
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

/// Make a **private (fragment-keyed) share link** to a file, as the actor
/// `secret` names, through the shared
/// [`fauna_client_share::ShareClient::create_private_link`] — the mint, the
/// key envelope sealed under a fresh link key, the registration, and the URL
/// revealed only against the nest's reply (`share-links.md` § The private-file
/// extension). The fixture for a viewer journey until an app's create control
/// routes to the private arm; never a re-derivation of the token or envelope.
///
/// `request` is the dag-cbor array `[manifest, filename, lifetime_secs]`:
/// `manifest` the file's canonical `ChunkManifest` in its sealed wire form (the
/// bytes uploaded to `POST /api/v1/manifests`, sealed under this owner's root
/// — `fauna_folder_seal_owner_file`), `filename` the name the envelope
/// carries. `out` receives `{"ok": [ShareRecord, url]}` or `{"err": RpcError}`.
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
        let (manifest_bytes, filename, lifetime_secs): (fauna_protocol::ByteBuf, String, u64) =
            decode_request!(call.req)?;
        let manifest: fauna_core::chunk::ChunkManifest =
            decode_request!(manifest_bytes.as_slice())?;
        let file = fauna_client_share::LinkFile {
            manifest_hash: fauna_core::data::ContentHash::of_raw(&manifest_bytes).digest(),
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
