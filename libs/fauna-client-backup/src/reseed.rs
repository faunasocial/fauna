//! The **re-seed ceremony driver** — the one place the standalone-restore
//! ceremony's three phases are put in order.
//!
//! Owner: `docs/goal/behavior/backup-destinations.md` § Third destination kind →
//! *Re-seed* (the ceremony, its authorization shape and the empty-target rule);
//! wire mechanics `docs/goal/architecture/segment-backup-protocol.md`
//! § Client-device custodian (pull) → *Restore*.
//!
//! # The three phases, and which half is this module's
//!
//! 1. **Enroll first.** The owner's ordinary sign-in on the target (claim or
//!    admission, then device registration) is the app's — there is no
//!    pre-enrollment write surface, so this driver is handed an already
//!    authenticated connection. What the ceremony adds to enrollment is the
//!    owner's `NestBackupKey` grant, which phase 3 unseals under; this driver
//!    issues it, first, before a byte moves.
//! 2. **Deliver into destination posture** — [`ReseedDeliveryLeg`]. The
//!    custodian re-seals its held corpus under the `NestBackupKey` root and
//!    pushes it over the existing byte + custody surfaces. The leg is a seam,
//!    not a call, because its production implementation
//!    (`fauna_sync_engine::reseed::ReseedDelivery`) opens a native sealed store
//!    this wasm-clean crate cannot name, and because a *surviving nest
//!    destination's* custody needs no push at all — the materialize half below
//!    is one shared design for both restore sources.
//! 3. **Materialize behind the owner's gesture** — one
//!    [`BackupClient::custody_materialize`] per delivered segment set (the
//!    account rails) first, then each covered folder in pages of the owner's
//!    re-home signatures ([`ReseedDeliveryLeg::sign_rehome`]) until the target
//!    owes nothing (`writer-signed-change-records.md` ruling (7)(a)). A folder
//!    set the host cannot sign for is held with the reason
//!    ([`SetRefusal::RehomeUnsigned`]) — never an unsigned request.
//!
//! # Why the order is the whole contract
//!
//! Grant before delivery, because a delivered corpus on a nest holding no grant
//! is an opaque destination nobody will ever materialize — correct, but a
//! half-state the owner cannot see. Delivery before materialize, because the
//! materialize verb reads what custody holds *now*: materializing a part-
//! delivered set is refused `custody_incomplete` at best. So a delivery failure
//! **stops** the ceremony before any materialize is sent.
//!
//! # Crash-safety and retry
//!
//! Every phase is safe to re-run from the top, which is the only retry this
//! driver offers: the grant replaces, delivery dedups by content address, and a
//! materialize that already succeeded answers `target_not_empty` before
//! reading a byte. That last point is why [`SetOutcome::AlreadyLive`] is an
//! outcome, not an error — on a retry it is the normal answer for the sets the
//! previous attempt finished. Nothing here deletes anything, on either side.

use async_trait::async_trait;
use fauna_protocol::{MaybeSendSync, RpcError, RpcErrorClass, RpcRequester};

use crate::BackupClient;

/// Which materialize arm a delivered set belongs to — the one fact the driver
/// needs about a set beyond its name, because the folder arm requires a display
/// name the segment arm must not be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetAxis {
    /// A reserved segment set (`__mail`, `__post`, `__calendar`, `__card`) — the account rails.
    Segment,
    /// A covered-folder mirror set (`__folder/<nest>/<id>`).
    Folder,
}

/// One custody set phase 2 left on the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredSet {
    /// The reserved set name the custody rows carry — exactly what
    /// `fauna.backup.custody.materialize` takes.
    pub set_name: String,
    pub axis: SetAxis,
    /// A covered folder's display name — what the folder materialize arm
    /// restores it under, and the one thing custody on the target never
    /// carries. The delivery leg reads it from the custodian's own local store,
    /// where the pull recorded it off the owner's coverage listing
    /// (`segment-backup-protocol.md` § Client-device custodian (pull) →
    /// *Restore* → *Where a restored folder's name comes from*); it is the
    /// ONE source every caller shares, which is why the driver takes no name
    /// map from its caller. `None` on every segment set, and on a folder set
    /// this device holds no name for — which the driver reports
    /// [`SetRefusal::FolderUnnamed`] rather than guessing.
    pub folder_display_name: Option<String>,
    /// A covered folder's address and sealed label, read from the same store
    /// entry as [`Self::folder_display_name`] (`CoveredFolder::{name_hash,
    /// name_sealed}`). When present the folder materialize names its target by
    /// the hash (`CustodyMaterializeRequest::folder_name_hash`). `None` on a
    /// segment set and on a folder set the store recorded no label for.
    pub folder_label: Option<FolderLabel>,
}

/// A covered folder's address and sealed label as its custodian recorded them
/// off the owner's coverage listing: the set's `name_hash` and its
/// `name_sealed`, verbatim. The seal is salted by the name, never by a row id,
/// so it opens wherever the same owner restores the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderLabel {
    /// The set's `name_hash` — what a re-seed names its target by.
    pub name_hash: [u8; 32],
    /// The set's `name_sealed`, verbatim.
    pub name_sealed: Vec<u8>,
}

/// What phase 2 moved, in the terms the driver and the owner need.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveredCorpus {
    /// Every set delivered, in delivery order.
    pub sets: Vec<DeliveredSet>,
    /// Segments delivered without a sidecar (the store never backfilled one) —
    /// the segment set they belong to will be refused `custody_incomplete`, and
    /// the owner is shown why rather than a bare refusal.
    pub sidecarless_segments: Vec<u32>,
    /// Covered-folder paths delivered without a sealed name.
    pub folder_paths_without_seal: Vec<String>,
    /// Sum of the delivered paths' plaintext sizes.
    pub plaintext_bytes: u64,
}

/// Phase 2 — deliver this device's held corpus to the target as ordinary
/// custody.
///
/// The error is flattened to `String` at this seam, like
/// [`crate::trust::BackupNestSeam`]'s: nothing the driver does depends on the
/// cause beyond "delivery did not complete", and the text is what the owner is
/// shown.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait ReseedDeliveryLeg: MaybeSendSync {
    async fn deliver(&self) -> Result<DeliveredCorpus, String>;

    /// The owner's signatures over every row the folder materialize arm will
    /// re-home from `set` into the live set named by its display name — the
    /// leg holds the store's rows and the host's signer, and resolves the
    /// target's nonce by that name as every other record's is
    /// (`writer-signed-change-records.md` ruling (7)(a)(ii)). Called once per
    /// named folder set, after delivery; [`FolderRehome::Unsigned`] when the
    /// host holds no signer or no nonce for the target.
    async fn sign_rehome(&self, set: &DeliveredSet) -> FolderRehome;
}

/// What [`ReseedDeliveryLeg::sign_rehome`] produced for one folder set.
#[derive(Debug, Clone, PartialEq)]
pub enum FolderRehome {
    /// The key every signature verifies under, and one signature per re-homed
    /// row keyed by its `path_hash` — sent in pages of
    /// [`fauna_protocol::backup::MATERIALIZE_REHOME_PAGE`].
    Signed {
        signer_key: Vec<u8>,
        signatures: Vec<fauna_protocol::backup::RehomeSignature>,
    },
    /// The host cannot sign for this set, and why — the set is held as
    /// [`SetRefusal::RehomeUnsigned`], never sent unsigned.
    Unsigned { reason: String },
}

/// Why the target refused one set, by the wire code the nest chose — each one
/// names a different next step, which is the reason they are distinct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetRefusal {
    /// `target_not_fresh` — the named folder already has an audience.
    TargetNotFresh,
    /// `custody_incomplete` — the corpus on the target is not whole yet.
    CustodyIncomplete,
    /// `custody_unsealed` — a folder path arrived without its sealed name.
    CustodyUnsealed,
    /// `storage_quota_exceeded` — the corpus outgrows this nest's quota; raised
    /// in the admin app like any other quota refusal.
    QuotaExceeded,
    /// `not_enrolled` — the key grant is missing on the target.
    NotEnrolled,
    /// A folder set this device has no display name for. Never sent: the nest
    /// would refuse it as malformed, and the driver must not guess a name.
    FolderUnnamed,
    /// `target_missing` — the live set to re-home into does not exist on the
    /// target: the ceremony's pre-create did not run or did not land.
    TargetMissing,
    /// A folder set this host could not sign the re-homed rows of (no signer,
    /// or no nonce for the target). Never sent: the nest refuses an unsigned
    /// re-home, and the driver never asks it to.
    RehomeUnsigned,
    /// Any other rejection, by its wire code.
    Other { code: String },
}

/// What happened to one delivered set in phase 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetOutcome {
    /// Materialized now: the target serves the set as a live account.
    Materialized { segments: Vec<u32>, records: u64 },
    /// `target_not_empty` — the target already holds live records for this
    /// scope. On a retry this is the set an earlier attempt finished; on a
    /// first attempt it means the owner pointed at a lived-in nest. Either way
    /// nothing was written and there is no force arm.
    AlreadyLive,
    /// Refused for a reason the owner can act on.
    Refused { refusal: SetRefusal, detail: String },
}

/// One set's line in the ceremony's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetResult {
    pub set: DeliveredSet,
    pub outcome: SetOutcome,
}

/// The ceremony's result, once delivery completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReseedOutcome {
    pub delivered: DeliveredCorpus,
    /// One entry per delivered set, in materialize order.
    pub sets: Vec<SetResult>,
}

impl ReseedOutcome {
    /// `true` when every delivered set is live on the target and nothing was
    /// delivered with a gap. The one bit a UI may render as "restored" — a
    /// corpus with a hole reported as whole is exactly what this driver exists
    /// to prevent.
    pub fn is_whole(&self) -> bool {
        !self.sets.is_empty()
            && self.delivered.sidecarless_segments.is_empty()
            && self.delivered.folder_paths_without_seal.is_empty()
            && self.sets.iter().all(|s| {
                matches!(
                    s.outcome,
                    SetOutcome::Materialized { .. } | SetOutcome::AlreadyLive
                )
            })
    }

    /// Mail records made live by this run, across every segment set.
    pub fn records_materialized(&self) -> u64 {
        self.sets
            .iter()
            .map(|s| match s.outcome {
                SetOutcome::Materialized { records, .. } => records,
                _ => 0,
            })
            .sum()
    }
}

/// Why the ceremony stopped before its end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReseedError {
    /// Phase 1 — the target refused or could not take the key grant. Nothing
    /// was delivered.
    Grant(String),
    /// Phase 2 — delivery did not complete. Nothing was materialized; what did
    /// land is ordinary custody, and re-running resumes by content address.
    Delivery(String),
    /// Phase 3 — the connection failed mid-materialize, so this set's result is
    /// unknown. Re-run the ceremony: a set that did flip answers `AlreadyLive`.
    Transport { set_name: String, detail: String },
}

impl std::fmt::Display for ReseedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Grant(e) => write!(f, "re-seed: the target refused the backup key grant: {e}"),
            Self::Delivery(e) => write!(f, "re-seed: delivery did not complete: {e}"),
            Self::Transport { set_name, detail } => {
                write!(
                    f,
                    "re-seed: materializing {set_name} lost the connection: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for ReseedError {}

fn classify(rpc: &RpcError) -> SetOutcome {
    let detail = rpc.detail_or_code();
    let refusal = match rpc.code.as_str() {
        RpcError::CODE_BACKUP_TARGET_NOT_EMPTY => return SetOutcome::AlreadyLive,
        RpcError::CODE_BACKUP_TARGET_NOT_FRESH => SetRefusal::TargetNotFresh,
        RpcError::CODE_BACKUP_CUSTODY_INCOMPLETE => SetRefusal::CustodyIncomplete,
        RpcError::CODE_BACKUP_CUSTODY_UNSEALED => SetRefusal::CustodyUnsealed,
        RpcError::CODE_SYNC_STORAGE_QUOTA_EXCEEDED => SetRefusal::QuotaExceeded,
        RpcError::CODE_BACKUP_NOT_ENROLLED => SetRefusal::NotEnrolled,
        RpcError::CODE_BACKUP_TARGET_MISSING => SetRefusal::TargetMissing,
        other => SetRefusal::Other {
            code: other.to_string(),
        },
    };
    SetOutcome::Refused { refusal, detail }
}

/// Materialize one named folder set in pages of the owner's re-home
/// signatures, until the target owes nothing or the signatures run out.
///
/// `Err` is a transport fault (the set's result unknown). A page refused
/// `target_not_empty` on the FIRST page is the set an earlier attempt finished
/// ([`SetOutcome::AlreadyLive`]); custody rows still owed once every signature
/// was sent are rows this device did not sign — reported, never counted whole.
async fn materialize_folder_pages<R, L>(
    target: &BackupClient<R>,
    leg: &L,
    set: &DeliveredSet,
    display_name: String,
) -> Result<SetOutcome, R::Error>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    L: ReseedDeliveryLeg + ?Sized,
{
    let (signer_key, signatures) = match leg.sign_rehome(set).await {
        FolderRehome::Signed {
            signer_key,
            signatures,
        } if !signatures.is_empty() => (signer_key, signatures),
        FolderRehome::Signed { .. } => {
            return Ok(SetOutcome::Refused {
                refusal: SetRefusal::RehomeUnsigned,
                detail: format!("no re-homable row of {} was signed", set.set_name),
            });
        }
        FolderRehome::Unsigned { reason } => {
            return Ok(SetOutcome::Refused {
                refusal: SetRefusal::RehomeUnsigned,
                detail: reason,
            });
        }
    };

    let mut records = 0;
    let mut remaining = None;
    for (i, page) in signatures
        .chunks(fauna_protocol::backup::MATERIALIZE_REHOME_PAGE)
        .enumerate()
    {
        // A labelled set is named by its address alone, so its plaintext name
        // stays on this device (`path-sealing.md` § the set-name plane); only
        // a set the store holds no label for falls back to the name.
        let folder_name_hash = set
            .folder_label
            .as_ref()
            .map(|label| fauna_protocol::ByteBuf::from(label.name_hash.to_vec()));
        let req = fauna_protocol::backup::CustodyMaterializeRequest {
            set_name: set.set_name.clone(),
            folder_display_name: folder_name_hash.is_none().then(|| display_name.clone()),
            folder_name_hash,
            signer_key: Some(fauna_protocol::ByteBuf::from(signer_key.clone())),
            signatures: page.to_vec(),
            ..Default::default()
        };
        match target.custody_materialize(req).await {
            Ok(reply) => {
                records = reply.records;
                remaining = reply.remaining;
                if reply.remaining == Some(0) {
                    break;
                }
            }
            Err(e) => match e.as_rpc_error() {
                Some(rpc) if i > 0 && rpc.code == RpcError::CODE_BACKUP_TARGET_NOT_EMPTY => {
                    // A later page cannot find foreign rows the first page's
                    // classifier admitted unless the folder was written to in
                    // between — a lived-in target now, not ours to finish.
                    return Ok(SetOutcome::Refused {
                        refusal: SetRefusal::Other {
                            code: rpc.code.clone(),
                        },
                        detail: rpc.detail_or_code(),
                    });
                }
                Some(rpc) => return Ok(classify(rpc)),
                None => return Err(e),
            },
        }
    }
    match remaining {
        Some(owed) if owed > 0 => Ok(SetOutcome::Refused {
            refusal: SetRefusal::CustodyIncomplete,
            detail: format!(
                "{owed} custody row(s) of {} were not signed by this device",
                set.set_name
            ),
        }),
        _ => Ok(SetOutcome::Materialized {
            segments: Vec::new(),
            records,
        }),
    }
}

/// Run the ceremony against `target` — the nest being seeded, reached over the
/// owner's own authenticated connection.
///
/// `nest_backup_key` is the owner's 32-byte seed-derived `NestBackupKey`, the
/// same root `leg` re-seals under. A covered folder's display name is the
/// leg's to supply ([`DeliveredSet::folder_display_name`]); a folder set
/// delivered without one is reported [`SetRefusal::FolderUnnamed`] and never
/// sent.
pub async fn run_reseed<R, L>(
    target: &BackupClient<R>,
    nest_backup_key: Vec<u8>,
    leg: &L,
) -> Result<ReseedOutcome, ReseedError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    L: ReseedDeliveryLeg + ?Sized,
{
    // Phase 1 — the grant. `ok: false` is a refusal in all but transport.
    let grant = target
        .nest_key_grant(nest_backup_key)
        .await
        .map_err(|e| ReseedError::Grant(e.to_string()))?;
    if !grant.ok {
        return Err(ReseedError::Grant(
            "the target did not store the backup key".into(),
        ));
    }

    // Phase 2 — delivery. A failure stops here: materializing a part-delivered
    // corpus is exactly the torn state the ordering exists to avoid.
    let delivered = leg.deliver().await.map_err(ReseedError::Delivery)?;

    // Phase 3 — segment sets first, the account rails, then folders; stable
    // within each axis so a retry walks the same order.
    let mut order: Vec<&DeliveredSet> = delivered.sets.iter().collect();
    order.sort_by_key(|s| match s.axis {
        SetAxis::Segment => 0,
        SetAxis::Folder => 1,
    });

    let mut sets = Vec::with_capacity(order.len());
    for set in order {
        let folder_display_name = match set.axis {
            SetAxis::Segment => None,
            SetAxis::Folder => match &set.folder_display_name {
                Some(name) => Some(name.clone()),
                None => {
                    sets.push(SetResult {
                        set: set.clone(),
                        outcome: SetOutcome::Refused {
                            refusal: SetRefusal::FolderUnnamed,
                            detail: format!("no display name for {}", set.set_name),
                        },
                    });
                    continue;
                }
            },
        };
        let result = match folder_display_name {
            Some(display_name) => materialize_folder_pages(target, leg, set, display_name).await,
            None => target
                .custody_materialize(fauna_protocol::backup::CustodyMaterializeRequest {
                    set_name: set.set_name.clone(),
                    ..Default::default()
                })
                .await
                .map(|reply| SetOutcome::Materialized {
                    segments: reply.segments,
                    records: reply.records,
                }),
        };
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) => match e.as_rpc_error() {
                Some(rpc) => classify(rpc),
                None => {
                    return Err(ReseedError::Transport {
                        set_name: set.set_name.clone(),
                        detail: e.to_string(),
                    });
                }
            },
        };
        sets.push(SetResult {
            set: set.clone(),
            outcome,
        });
    }

    Ok(ReseedOutcome { delivered, sets })
}

impl SetRefusal {
    /// The refusal's code: the nest's own wire code for every refusal the nest
    /// chose, and a local one for [`Self::FolderUnnamed`], which the driver
    /// decides without asking the nest. What a process boundary carries (the
    /// desktop agent runs the ceremony and the app renders it —
    /// `backup-destinations.md` § *Where the ceremony runs*), so the two ends
    /// classify through one table.
    pub fn code(&self) -> &str {
        match self {
            Self::TargetNotFresh => RpcError::CODE_BACKUP_TARGET_NOT_FRESH,
            Self::CustodyIncomplete => RpcError::CODE_BACKUP_CUSTODY_INCOMPLETE,
            Self::CustodyUnsealed => RpcError::CODE_BACKUP_CUSTODY_UNSEALED,
            Self::QuotaExceeded => RpcError::CODE_SYNC_STORAGE_QUOTA_EXCEEDED,
            Self::NotEnrolled => RpcError::CODE_BACKUP_NOT_ENROLLED,
            Self::FolderUnnamed => FOLDER_UNNAMED_CODE,
            Self::TargetMissing => RpcError::CODE_BACKUP_TARGET_MISSING,
            Self::RehomeUnsigned => REHOME_UNSIGNED_CODE,
            Self::Other { code } => code,
        }
    }

    /// The inverse of [`Self::code`]. An unknown code is kept verbatim as
    /// [`Self::Other`], never guessed into a known remedy.
    pub fn from_code(code: &str) -> Self {
        match code {
            RpcError::CODE_BACKUP_TARGET_NOT_FRESH => Self::TargetNotFresh,
            RpcError::CODE_BACKUP_CUSTODY_INCOMPLETE => Self::CustodyIncomplete,
            RpcError::CODE_BACKUP_CUSTODY_UNSEALED => Self::CustodyUnsealed,
            RpcError::CODE_SYNC_STORAGE_QUOTA_EXCEEDED => Self::QuotaExceeded,
            RpcError::CODE_BACKUP_NOT_ENROLLED => Self::NotEnrolled,
            FOLDER_UNNAMED_CODE => Self::FolderUnnamed,
            RpcError::CODE_BACKUP_TARGET_MISSING => Self::TargetMissing,
            REHOME_UNSIGNED_CODE => Self::RehomeUnsigned,
            other => Self::Other {
                code: other.to_string(),
            },
        }
    }

    /// The i18n key of the remedy this refusal names: each refusal is distinct
    /// because its next step is (the enum's own docs).
    fn remedy_key(&self) -> &'static str {
        match self {
            Self::TargetNotFresh => "backups.reseed_refused_target_not_fresh",
            Self::CustodyIncomplete => "backups.reseed_refused_custody_incomplete",
            Self::CustodyUnsealed => "backups.reseed_refused_custody_unsealed",
            Self::QuotaExceeded => "backups.reseed_refused_quota_exceeded",
            Self::NotEnrolled => "backups.reseed_refused_not_enrolled",
            Self::FolderUnnamed => "backups.reseed_refused_folder_unnamed",
            Self::TargetMissing => "backups.reseed_refused_target_missing",
            Self::RehomeUnsigned => "backups.reseed_refused_rehome_unsigned",
            Self::Other { .. } => "backups.reseed_refused_other",
        }
    }
}

/// [`SetRefusal::FolderUnnamed`]'s code. Local: the nest never sends it.
pub const FOLDER_UNNAMED_CODE: &str = "folder_unnamed";

/// [`SetRefusal::RehomeUnsigned`]'s code. Local: the driver holds the set and
/// the nest never sees it.
pub const REHOME_UNSIGNED_CODE: &str = "rehome_unsigned";

/// How a delivered set is named to the owner. `__mail` is "Mail"; a covered
/// folder is its display name; any other set shows its set name, which reaches
/// the owner only for a folder this device could not name.
fn set_label(set: &DeliveredSet) -> String {
    match (set.axis, set.set_name.as_str()) {
        (SetAxis::Segment, "__mail") => "backups.reseed_set_mail".to_string(),
        (SetAxis::Folder, _) => set
            .folder_display_name
            .clone()
            .unwrap_or_else(|| set.set_name.clone()),
        _ => set.set_name.clone(),
    }
}

/// The `backup-destination-reseed-result` view's text: the verdict first, then
/// one line per set in materialize order, then one line per delivery gap.
///
/// The verdict is [`ReseedOutcome::is_whole`], never re-derived. Keys, not
/// strings, so every app resolves them through its own table: resolve each
/// line with [`fauna_core::localized::LocalizedText::resolve_nested`], because
/// the set name inside a line is itself a key.
pub fn result_lines(outcome: &ReseedOutcome) -> Vec<fauna_core::localized::LocalizedText> {
    use fauna_core::localized::LocalizedText;
    let mut lines = vec![LocalizedText::key(if outcome.is_whole() {
        "backups.reseed_result_whole"
    } else {
        "backups.reseed_result_incomplete"
    })];
    for set in &outcome.sets {
        let name = set_label(&set.set);
        lines.push(match &set.outcome {
            SetOutcome::Materialized { records, .. } => LocalizedText::key_args(
                "backups.reseed_set_restored",
                [("set", name), ("count", records.to_string())],
            ),
            SetOutcome::AlreadyLive => {
                LocalizedText::key_arg("backups.reseed_set_already_restored", "set", name)
            }
            SetOutcome::Refused { refusal, .. } => LocalizedText::key_args(
                "backups.reseed_set_refused",
                [
                    ("set", name),
                    ("remedy", refusal.remedy_key().to_string()),
                    ("code", refusal.code().to_string()),
                ],
            ),
        });
    }
    let delivered = &outcome.delivered;
    if !delivered.sidecarless_segments.is_empty() {
        lines.push(LocalizedText::key_arg(
            "backups.reseed_gap_sidecarless_segments",
            "count",
            delivered.sidecarless_segments.len().to_string(),
        ));
    }
    if !delivered.folder_paths_without_seal.is_empty() {
        lines.push(LocalizedText::key_arg(
            "backups.reseed_gap_unnamed_files",
            "count",
            delivered.folder_paths_without_seal.len().to_string(),
        ));
    }
    lines
}

/// Which delivery leg would move a destination's copy onto a rebuilt nest.
///
/// Both restore sources end in the same materialize (`backup-destinations.md`
/// § Re-seed, "one shared materialization for both restore sources"); they
/// differ only in how the corpus reaches the target, and the destination row's
/// kind decides which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReseedLeg {
    /// This device's own sealed custodian store: re-sealed and pushed from here.
    ThisDeviceStore,
    /// A surviving nest destination's custody, pulled back by the owner's app.
    /// Not built yet (`segment-backup-protocol.md` § *The nest-held
    /// pull-back*), so no app offers it.
    NestHeldPullBack,
}

impl ReseedLeg {
    /// Whether this build can run the leg.
    pub fn is_built(self) -> bool {
        matches!(self, Self::ThisDeviceStore)
    }
}

/// One place a rebuilt nest's content could come back from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReseedSource {
    /// The destination row it came from; `None` for an orphaned store, which
    /// no row names.
    pub destination_id: Option<String>,
    pub leg: ReseedLeg,
}

/// Every restore source the owner could pick, in the page's row order: this
/// device's orphaned store first, when there is one, then each destination row
/// whose copy some leg could deliver.
///
/// A client-device row is a source only when it names **this** device, since
/// only the device holding a store can push from it. A nest row is always a
/// source, typed [`ReseedLeg::NestHeldPullBack`], so that the pull-back leg
/// plugs in by becoming built rather than by the list growing a new shape. An
/// [`fauna_core::data::DestinationKind::Inert`] row is never a source.
pub fn reseed_sources(
    rows: &[fauna_core::data::BackupDestination],
    this_device_id: &str,
    orphaned_store: bool,
) -> Vec<ReseedSource> {
    use fauna_core::data::DestinationKind;
    let me = this_device_id.trim();
    let mut sources = Vec::new();
    if orphaned_store {
        sources.push(ReseedSource {
            destination_id: None,
            leg: ReseedLeg::ThisDeviceStore,
        });
    }
    for row in rows {
        let leg = match row.kind_view() {
            DestinationKind::Nest { .. } => ReseedLeg::NestHeldPullBack,
            DestinationKind::ClientDevice { device_id, .. }
                if !me.is_empty() && device_id.trim() == me =>
            {
                ReseedLeg::ThisDeviceStore
            }
            _ => continue,
        };
        sources.push(ReseedSource {
            destination_id: Some(row.destination_id.clone()),
            leg,
        });
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_protocol::backup::{
        CustodyMaterializeReply, CustodyMaterializeRequest, NestKeyGrantReply, NestKeyGrantRequest,
    };
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Every call the ceremony made, in order: the grant, the delivery, each
    /// materialize — one log across the nest and the leg, because the ORDER
    /// across them is the contract under test.
    type Log = Mutex<Vec<String>>;

    #[derive(Debug)]
    enum FakeErr {
        Rejected(RpcError),
        Transport,
    }

    impl std::fmt::Display for FakeErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Rejected(e) => write!(f, "rejected {}", e.code),
                Self::Transport => write!(f, "disconnected"),
            }
        }
    }

    impl RpcErrorClass for FakeErr {
        fn is_rejection(&self) -> bool {
            matches!(self, Self::Rejected(_))
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            match self {
                Self::Rejected(e) => Some(e),
                Self::Transport => None,
            }
        }
    }

    /// Scripted target: a grant answer, then one materialize answer per set
    /// name (`Ok(records)` or a rejection code, `None` for a transport fault).
    struct FakeTarget<'a> {
        log: &'a Log,
        /// Custody rows still owed per folder set: each page subtracts what it
        /// carries, and the reply's `remaining` is what is left (`0` for a set
        /// not listed).
        owed: Mutex<BTreeMap<String, u64>>,
        grant_ok: bool,
        materialize: BTreeMap<String, Option<Result<u64, &'static str>>>,
    }

    impl RpcRequester for &FakeTarget<'_> {
        type Error = FakeErr;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).unwrap();
            let reply = match kind {
                crate::KIND_NEST_KEY_GRANT => {
                    let req: NestKeyGrantRequest = fauna_protocol::decode_strict(&bytes).unwrap();
                    self.log
                        .lock()
                        .unwrap()
                        .push(format!("grant:{}", req.nest_backup_key.len()));
                    fauna_protocol::encode_canonical(&NestKeyGrantReply {
                        ok: self.grant_ok,
                        ..Default::default()
                    })
                    .unwrap()
                }
                crate::KIND_CUSTODY_MATERIALIZE => {
                    let req: CustodyMaterializeRequest =
                        fauna_protocol::decode_strict(&bytes).unwrap();
                    let sigs = if req.signatures.is_empty() {
                        String::new()
                    } else {
                        format!(":{}sig", req.signatures.len())
                    };
                    let hash = req
                        .folder_name_hash
                        .as_ref()
                        .map(|h| format!("#{}", hex::encode(&h[..])))
                        .unwrap_or_default();
                    self.log.lock().unwrap().push(format!(
                        "materialize:{}:{}{hash}{sigs}",
                        req.set_name,
                        req.folder_display_name.as_deref().unwrap_or("-")
                    ));
                    let folder_axis =
                        req.folder_display_name.is_some() || req.folder_name_hash.is_some();
                    let remaining = folder_axis.then(|| {
                        let mut owed = self.owed.lock().unwrap();
                        let left = owed.entry(req.set_name.clone()).or_insert(0);
                        *left = left.saturating_sub(req.signatures.len() as u64);
                        *left
                    });
                    match self.materialize.get(&req.set_name).cloned().flatten() {
                        None => return Err(FakeErr::Transport),
                        Some(Err(code)) => {
                            return Err(FakeErr::Rejected(
                                RpcError::new(code, "k")
                                    .with_details_text(format!("detail for {code}")),
                            ));
                        }
                        Some(Ok(records)) => {
                            fauna_protocol::encode_canonical(&CustodyMaterializeReply {
                                segments: vec![1, 2],
                                records,
                                custody_redundant: true,
                                remaining,
                                ..Default::default()
                            })
                            .unwrap()
                        }
                    }
                }
                other => panic!("the ceremony spoke an unexpected kind: {other}"),
            };
            Ok(fauna_protocol::decode_strict(&reply).unwrap())
        }
    }

    struct FakeLeg<'a> {
        log: &'a Log,
        /// How many rows each folder set's re-home signs.
        rows: usize,
        /// `Some(reason)`: the host cannot sign.
        unsigned: Option<&'static str>,
        result: Result<DeliveredCorpus, String>,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    impl ReseedDeliveryLeg for FakeLeg<'_> {
        async fn deliver(&self) -> Result<DeliveredCorpus, String> {
            self.log.lock().unwrap().push("deliver".into());
            self.result.clone()
        }

        async fn sign_rehome(&self, set: &DeliveredSet) -> FolderRehome {
            self.log
                .lock()
                .unwrap()
                .push(format!("sign:{}", set.set_name));
            match self.unsigned {
                Some(reason) => FolderRehome::Unsigned {
                    reason: reason.into(),
                },
                None => FolderRehome::Signed {
                    signer_key: vec![1; 32],
                    signatures: (0..self.rows)
                        .map(|i| fauna_protocol::backup::RehomeSignature {
                            path_hash: fauna_protocol::ByteBuf::from({
                                let mut h = vec![0u8; 32];
                                h[..8].copy_from_slice(&(i as u64).to_le_bytes());
                                h
                            }),
                            signature: fauna_protocol::ByteBuf::from(vec![2; 64]),
                            ..Default::default()
                        })
                        .collect(),
                },
            }
        }
    }

    fn set(name: &str, axis: SetAxis) -> DeliveredSet {
        DeliveredSet {
            set_name: name.into(),
            axis,
            folder_display_name: None,
            folder_label: None,
        }
    }

    /// A covered-folder set the leg delivered with its name, as the production
    /// leg mints one from the store.
    fn named(set_name: &str, display: &str) -> DeliveredSet {
        DeliveredSet {
            set_name: set_name.into(),
            axis: SetAxis::Folder,
            folder_display_name: Some(display.into()),
            folder_label: None,
        }
    }

    fn corpus(sets: Vec<DeliveredSet>) -> DeliveredCorpus {
        DeliveredCorpus {
            sets,
            plaintext_bytes: 10,
            ..Default::default()
        }
    }

    const FOLDER: &str = "__folder/aa/7";

    fn log(l: &Log) -> Vec<String> {
        l.lock().unwrap().clone()
    }

    /// The contract in one assertion: grant, then deliver, then materialize —
    /// segment sets before folders even when delivery listed a folder first,
    /// and the folder carrying the name the leg read off this device's store.
    #[test]
    fn grant_then_deliver_then_materialize_segments_before_folders() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Default::default(),
            grant_ok: true,
            materialize: BTreeMap::from([
                ("__mail".to_string(), Some(Ok(2))),
                (FOLDER.to_string(), Some(Ok(0))),
            ]),
        };
        let leg = FakeLeg {
            log: &l,
            rows: 1,
            unsigned: None,
            result: Ok(corpus(vec![
                named(FOLDER, "Photos"),
                set("__mail", SetAxis::Segment),
            ])),
        };
        let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();

        assert_eq!(
            log(&l),
            [
                "grant:32",
                "deliver",
                "materialize:__mail:-",
                "sign:__folder/aa/7",
                "materialize:__folder/aa/7:Photos:1sig",
            ]
        );
        assert!(out.is_whole());
        assert_eq!(out.records_materialized(), 2);
    }

    /// The folder arm pages the owner's signatures until the target owes
    /// nothing — every row carried once, each request within the page
    /// (`writer-signed-change-records.md` ruling (7)(a)(ii)).
    #[test]
    fn a_folder_materializes_in_pages_until_nothing_is_owed() {
        let page = fauna_protocol::backup::MATERIALIZE_REHOME_PAGE;
        let rows = page * 2 + 3;
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Mutex::new(BTreeMap::from([(FOLDER.to_string(), rows as u64)])),
            grant_ok: true,
            materialize: BTreeMap::from([(FOLDER.to_string(), Some(Ok(rows as u64)))]),
        };
        let leg = FakeLeg {
            log: &l,
            rows,
            unsigned: None,
            result: Ok(corpus(vec![named(FOLDER, "Photos")])),
        };
        let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();
        assert_eq!(
            log(&l),
            [
                "grant:32".to_string(),
                "deliver".into(),
                "sign:__folder/aa/7".into(),
                format!("materialize:__folder/aa/7:Photos:{page}sig"),
                format!("materialize:__folder/aa/7:Photos:{page}sig"),
                "materialize:__folder/aa/7:Photos:3sig".into(),
            ]
        );
        assert!(out.is_whole());
        assert_eq!(
            out.sets[0].outcome,
            SetOutcome::Materialized {
                segments: vec![],
                records: rows as u64
            }
        );
    }

    /// A folder set the custodian recorded a label for names its target by the
    /// label's hash on every page, so the nest resolves the set by address
    /// rather than by a plaintext name it may no longer hold — and the
    /// plaintext name never rides beside it.
    #[test]
    fn a_labelled_folder_set_names_its_target_by_hash_on_every_page() {
        let page = fauna_protocol::backup::MATERIALIZE_REHOME_PAGE;
        let rows = page + 1;
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Mutex::new(BTreeMap::from([(FOLDER.to_string(), rows as u64)])),
            grant_ok: true,
            materialize: BTreeMap::from([(FOLDER.to_string(), Some(Ok(rows as u64)))]),
        };
        let labelled = DeliveredSet {
            folder_label: Some(FolderLabel {
                name_hash: [0xab; 32],
                name_sealed: vec![1, 2, 3],
            }),
            ..named(FOLDER, "Photos")
        };
        let leg = FakeLeg {
            log: &l,
            rows,
            unsigned: None,
            result: Ok(corpus(vec![labelled])),
        };
        let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();
        let hash = "ab".repeat(32);
        assert_eq!(
            log(&l)[3..],
            [
                format!("materialize:__folder/aa/7:-#{hash}:{page}sig"),
                format!("materialize:__folder/aa/7:-#{hash}:1sig"),
            ]
        );
        assert!(out.is_whole());
    }

    /// Custody still owed once every signature was sent is a set this device
    /// did not sign whole — reported, never counted restored.
    #[test]
    fn rows_still_owed_after_the_last_page_are_not_whole() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Mutex::new(BTreeMap::from([(FOLDER.to_string(), 5)])),
            grant_ok: true,
            materialize: BTreeMap::from([(FOLDER.to_string(), Some(Ok(4)))]),
        };
        let leg = FakeLeg {
            log: &l,
            rows: 4,
            unsigned: None,
            result: Ok(corpus(vec![named(FOLDER, "Photos")])),
        };
        let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();
        assert!(!out.is_whole());
        assert!(
            matches!(
                &out.sets[0].outcome,
                SetOutcome::Refused { refusal: SetRefusal::CustodyIncomplete, detail }
                    if detail.contains("1 custody row")
            ),
            "{:?}",
            out.sets[0].outcome
        );
    }

    /// A host that cannot sign for a folder set holds it with the reason —
    /// never an unsigned request (ruling (7)(a)(iii)) — and the ceremony is
    /// not whole.
    #[test]
    fn a_folder_the_host_cannot_sign_is_held_typed_and_never_sent() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Default::default(),
            grant_ok: true,
            materialize: BTreeMap::from([("__mail".to_string(), Some(Ok(1)))]),
        };
        let leg = FakeLeg {
            log: &l,
            rows: 1,
            unsigned: Some("no set nonce for Photos"),
            result: Ok(corpus(vec![
                set("__mail", SetAxis::Segment),
                named(FOLDER, "Photos"),
            ])),
        };
        let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();
        assert_eq!(
            log(&l),
            [
                "grant:32",
                "deliver",
                "materialize:__mail:-",
                "sign:__folder/aa/7"
            ],
            "the unsigned folder is never sent"
        );
        assert!(!out.is_whole());
        assert_eq!(
            out.sets[1].outcome,
            SetOutcome::Refused {
                refusal: SetRefusal::RehomeUnsigned,
                detail: "no set nonce for Photos".into(),
            }
        );
        let lines: Vec<String> = result_lines(&out)
            .iter()
            .map(|l| l.resolve_nested(|k| fauna_i18n::strings::lookup(k).map(str::to_string)))
            .collect();
        assert!(
            lines.iter().any(|l| l.contains("could not sign")),
            "the owner reads why: {lines:?}"
        );
    }

    /// A missing target is its own refusal, by the nest's typed code.
    #[test]
    fn a_missing_target_set_is_its_own_refusal() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Default::default(),
            grant_ok: true,
            materialize: BTreeMap::from([(
                FOLDER.to_string(),
                Some(Err(RpcError::CODE_BACKUP_TARGET_MISSING)),
            )]),
        };
        let leg = FakeLeg {
            log: &l,
            rows: 1,
            unsigned: None,
            result: Ok(corpus(vec![named(FOLDER, "Photos")])),
        };
        let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();
        assert!(matches!(
            out.sets[0].outcome,
            SetOutcome::Refused {
                refusal: SetRefusal::TargetMissing,
                ..
            }
        ));
    }

    /// A delivery failure stops the ceremony before any materialize: flipping a
    /// part-delivered corpus is the torn state the ordering exists to prevent.
    #[test]
    fn a_failed_delivery_materializes_nothing() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Default::default(),
            grant_ok: true,
            materialize: BTreeMap::new(),
        };
        let leg = FakeLeg {
            log: &l,
            rows: 1,
            unsigned: None,
            result: Err("byte plane refused".into()),
        };
        let err = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap_err();
        assert_eq!(err, ReseedError::Delivery("byte plane refused".into()));
        assert_eq!(log(&l), ["grant:32", "deliver"]);
    }

    /// A refused grant delivers nothing — a corpus on a nest holding no grant
    /// is a destination nobody will materialize.
    #[test]
    fn a_refused_grant_delivers_nothing() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Default::default(),
            grant_ok: false,
            materialize: BTreeMap::new(),
        };
        let leg = FakeLeg {
            log: &l,
            rows: 1,
            unsigned: None,
            result: Ok(corpus(vec![set("__mail", SetAxis::Segment)])),
        };
        let err = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap_err();
        assert!(matches!(err, ReseedError::Grant(_)), "{err:?}");
        assert_eq!(log(&l), ["grant:32"]);
    }

    /// `target_not_empty` is an outcome, not a failure — it is what a retry
    /// hears for a set the earlier attempt finished — and one set's refusal
    /// does not stop the next set: freshness is per scope.
    #[test]
    fn target_not_empty_reads_as_already_live_and_later_sets_still_run() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Default::default(),
            grant_ok: true,
            materialize: BTreeMap::from([
                (
                    "__mail".to_string(),
                    Some(Err(RpcError::CODE_BACKUP_TARGET_NOT_EMPTY)),
                ),
                (FOLDER.to_string(), Some(Ok(0))),
            ]),
        };
        let leg = FakeLeg {
            log: &l,
            rows: 1,
            unsigned: None,
            result: Ok(corpus(vec![
                set("__mail", SetAxis::Segment),
                named(FOLDER, "Photos"),
            ])),
        };
        let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();
        assert_eq!(out.sets[0].outcome, SetOutcome::AlreadyLive);
        assert!(matches!(
            out.sets[1].outcome,
            SetOutcome::Materialized { .. }
        ));
        assert!(out.is_whole());
        assert_eq!(out.records_materialized(), 0);
    }

    /// Each typed refusal maps to its own remedy, and none is whole.
    #[test]
    fn typed_refusals_keep_their_remedy_and_are_not_whole() {
        for (code, want) in [
            (
                RpcError::CODE_BACKUP_TARGET_NOT_FRESH,
                SetRefusal::TargetNotFresh,
            ),
            (
                RpcError::CODE_BACKUP_CUSTODY_INCOMPLETE,
                SetRefusal::CustodyIncomplete,
            ),
            (
                RpcError::CODE_BACKUP_CUSTODY_UNSEALED,
                SetRefusal::CustodyUnsealed,
            ),
            (
                RpcError::CODE_SYNC_STORAGE_QUOTA_EXCEEDED,
                SetRefusal::QuotaExceeded,
            ),
            (RpcError::CODE_BACKUP_NOT_ENROLLED, SetRefusal::NotEnrolled),
            (
                "fauna.backup.custody_unreadable",
                SetRefusal::Other {
                    code: "fauna.backup.custody_unreadable".into(),
                },
            ),
        ] {
            let l = Log::default();
            let target = FakeTarget {
                log: &l,
                owed: Default::default(),
                grant_ok: true,
                materialize: BTreeMap::from([("__mail".to_string(), Some(Err(code)))]),
            };
            let leg = FakeLeg {
                log: &l,
                rows: 1,
                unsigned: None,
                result: Ok(corpus(vec![set("__mail", SetAxis::Segment)])),
            };
            let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();
            assert_eq!(
                out.sets[0].outcome,
                SetOutcome::Refused {
                    refusal: want,
                    detail: format!("detail for {code}"),
                },
                "{code}"
            );
            assert!(!out.is_whole(), "{code}");
        }
    }

    /// A folder the leg delivered without a name — this device's store holds
    /// none for it — is never sent (the driver does not guess a label), and
    /// the segment set still runs.
    #[test]
    fn an_unnamed_folder_is_reported_and_never_sent() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Default::default(),
            grant_ok: true,
            materialize: BTreeMap::from([("__mail".to_string(), Some(Ok(1)))]),
        };
        let leg = FakeLeg {
            log: &l,
            rows: 1,
            unsigned: None,
            result: Ok(corpus(vec![
                set("__mail", SetAxis::Segment),
                set("__folder/bb/9", SetAxis::Folder),
            ])),
        };
        let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();
        assert_eq!(log(&l), ["grant:32", "deliver", "materialize:__mail:-"]);
        assert!(matches!(
            out.sets[1].outcome,
            SetOutcome::Refused {
                refusal: SetRefusal::FolderUnnamed,
                ..
            }
        ));
        assert!(!out.is_whole());
    }

    /// A transport fault mid-materialize stops with the set named — its result
    /// is unknown, and the retry is the whole ceremony.
    #[test]
    fn a_transport_fault_mid_materialize_names_the_set() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Default::default(),
            grant_ok: true,
            materialize: BTreeMap::from([("__mail".to_string(), None)]),
        };
        let leg = FakeLeg {
            log: &l,
            rows: 1,
            unsigned: None,
            result: Ok(corpus(vec![set("__mail", SetAxis::Segment)])),
        };
        let err = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap_err();
        assert!(
            matches!(&err, ReseedError::Transport { set_name, .. } if set_name == "__mail"),
            "{err:?}"
        );
    }

    /// A delivery with a gap is never whole, even when every set flipped: the
    /// sidecarless segment's records are not in what came back.
    #[test]
    fn a_delivery_gap_is_never_whole() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Default::default(),
            grant_ok: true,
            materialize: BTreeMap::from([("__mail".to_string(), Some(Ok(1)))]),
        };
        let mut c = corpus(vec![set("__mail", SetAxis::Segment)]);
        c.sidecarless_segments = vec![3];
        let leg = FakeLeg {
            log: &l,
            rows: 1,
            unsigned: None,
            result: Ok(c),
        };
        let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();
        assert!(!out.is_whole());
    }

    /// Nothing delivered is nothing restored — never "whole".
    #[test]
    fn an_empty_delivery_is_not_whole() {
        let l = Log::default();
        let target = FakeTarget {
            log: &l,
            owed: Default::default(),
            grant_ok: true,
            materialize: BTreeMap::new(),
        };
        let leg = FakeLeg {
            log: &l,
            rows: 1,
            unsigned: None,
            result: Ok(corpus(vec![])),
        };
        let out = block_on(run_reseed(&BackupClient::new(&target), vec![7; 32], &leg)).unwrap();
        assert!(!out.is_whole());
    }
}

#[cfg(test)]
mod shared_view_tests {
    use super::*;
    use fauna_core::data::{BackupDestination, DESTINATION_KIND_CLIENT_DEVICE};

    const ME: &str = "aa11";

    fn custodian(id: &str, device: &str) -> BackupDestination {
        BackupDestination {
            destination_id: id.into(),
            kind: DESTINATION_KIND_CLIENT_DEVICE.into(),
            custodian_device_id: Some(device.into()),
            ..Default::default()
        }
    }

    fn nest(id: &str) -> BackupDestination {
        BackupDestination {
            destination_id: id.into(),
            destination_nest_url: "https://backup.example".into(),
            ..Default::default()
        }
    }

    fn mail() -> DeliveredSet {
        DeliveredSet {
            set_name: "__mail".into(),
            axis: SetAxis::Segment,
            folder_display_name: None,
            folder_label: None,
        }
    }

    fn lookup(key: &str) -> Option<String> {
        fauna_i18n::strings::lookup(key).map(str::to_string)
    }

    fn rendered(outcome: &ReseedOutcome) -> Vec<String> {
        result_lines(outcome)
            .iter()
            .map(|l| l.resolve_nested(lookup))
            .collect()
    }

    #[test]
    fn every_refusal_round_trips_through_its_code() {
        for refusal in [
            SetRefusal::TargetNotFresh,
            SetRefusal::CustodyIncomplete,
            SetRefusal::CustodyUnsealed,
            SetRefusal::QuotaExceeded,
            SetRefusal::NotEnrolled,
            SetRefusal::FolderUnnamed,
            SetRefusal::TargetMissing,
            SetRefusal::RehomeUnsigned,
            SetRefusal::Other {
                code: "something_new".into(),
            },
        ] {
            assert_eq!(SetRefusal::from_code(refusal.code()), refusal);
        }
    }

    #[test]
    fn the_sources_list_this_devices_store_and_every_nest_row_but_no_one_elses_device() {
        let rows = vec![
            custodian("mine", ME),
            custodian("theirs", "bb22"),
            nest("off-site"),
            // A client-device row with no device id is Inert: nothing drives it.
            BackupDestination {
                destination_id: "inert".into(),
                kind: DESTINATION_KIND_CLIENT_DEVICE.into(),
                ..Default::default()
            },
        ];
        assert_eq!(
            reseed_sources(&rows, ME, true),
            vec![
                ReseedSource {
                    destination_id: None,
                    leg: ReseedLeg::ThisDeviceStore
                },
                ReseedSource {
                    destination_id: Some("mine".into()),
                    leg: ReseedLeg::ThisDeviceStore
                },
                ReseedSource {
                    destination_id: Some("off-site".into()),
                    leg: ReseedLeg::NestHeldPullBack
                },
            ]
        );
        // Only the custodian leg is built today.
        assert!(ReseedLeg::ThisDeviceStore.is_built());
        assert!(!ReseedLeg::NestHeldPullBack.is_built());
        // A device that cannot name itself claims no client-device row.
        assert_eq!(
            reseed_sources(&rows, " ", false),
            vec![ReseedSource {
                destination_id: Some("off-site".into()),
                leg: ReseedLeg::NestHeldPullBack
            }]
        );
    }

    #[test]
    fn a_whole_restore_says_so_and_counts_the_mail() {
        let outcome = ReseedOutcome {
            delivered: DeliveredCorpus {
                sets: vec![mail()],
                ..Default::default()
            },
            sets: vec![SetResult {
                set: mail(),
                outcome: SetOutcome::Materialized {
                    segments: vec![0],
                    records: 3,
                },
            }],
        };
        let lines = rendered(&outcome);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0], lookup("backups.reseed_result_whole").unwrap());
        assert!(
            lines[1].contains("Mail") && lines[1].contains('3'),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("backups.")), "{lines:?}");
    }

    #[test]
    fn a_refusal_and_a_gap_each_get_their_own_line_and_the_verdict_is_incomplete() {
        let outcome = ReseedOutcome {
            delivered: DeliveredCorpus {
                sets: vec![mail()],
                sidecarless_segments: vec![4, 5],
                ..Default::default()
            },
            sets: vec![SetResult {
                set: mail(),
                outcome: SetOutcome::Refused {
                    refusal: SetRefusal::QuotaExceeded,
                    detail: "over".into(),
                },
            }],
        };
        let lines = rendered(&outcome);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(
            lines[0],
            lookup("backups.reseed_result_incomplete").unwrap()
        );
        let remedy = lookup("backups.reseed_refused_quota_exceeded").unwrap();
        assert!(lines[1].contains(&remedy), "{lines:?}");
        assert!(lines[2].contains('2'), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("backups.")), "{lines:?}");
    }

    /// A restored folder is shown by its display name — the one the leg read
    /// off this device's store — and only a folder the store could not name
    /// falls back to its set name, on the line that says why.
    #[test]
    fn a_folder_is_named_to_the_owner_by_its_display_name() {
        let photos = DeliveredSet {
            set_name: "__folder/aa/7".into(),
            axis: SetAxis::Folder,
            folder_display_name: Some("Photos".into()),
            folder_label: None,
        };
        let nameless = DeliveredSet {
            set_name: "__folder/aa/9".into(),
            axis: SetAxis::Folder,
            folder_display_name: None,
            folder_label: None,
        };
        let outcome = ReseedOutcome {
            delivered: DeliveredCorpus {
                sets: vec![photos.clone(), nameless.clone()],
                ..Default::default()
            },
            sets: vec![
                SetResult {
                    set: photos,
                    outcome: SetOutcome::Materialized {
                        segments: vec![],
                        records: 12,
                    },
                },
                SetResult {
                    set: nameless,
                    outcome: SetOutcome::Refused {
                        refusal: SetRefusal::FolderUnnamed,
                        detail: "no display name".into(),
                    },
                },
            ],
        };
        let lines = rendered(&outcome);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines[1].contains("Photos") && !lines[1].contains("__folder"),
            "{lines:?}"
        );
        assert!(
            lines[2].contains("__folder/aa/9")
                && lines[2].contains(&lookup("backups.reseed_refused_folder_unnamed").unwrap()),
            "{lines:?}"
        );
    }
}
