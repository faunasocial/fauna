//! The command service: the `Send + Clone` handle every consumer holds, the
//! command it sends, and the store-thread side that serves a **local**
//! command at a pass's yield point (`account-data-plane.md` § The client-side
//! lifecycle, the pump bullet → *Commands and passes*).

use std::sync::Arc;

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{IntentDrainer, StateEntry};
use fauna_core::fleet_removal::{
    FleetMembersView, FleetRemovalRefusal, NestDeletion, PendingFleetRemoval,
};
use fauna_protocol::merge_policy::LwwStamp;
use fauna_protocol::scope::ContentScope;
use fauna_protocol::{RpcErrorClass, RpcRequester};
use tokio::sync::{mpsc, oneshot, watch};

use crate::account_state_plane::{AccountStatePlane, ItemId};
use crate::device_endpoints_writer::EndpointFacts;
use crate::observation_intake::{Observation, ObservationOutcome, record_observation};
use crate::preference_put::put_preference_local;
use crate::principal_custody::{EnrollmentRefusal, PrincipalBundleStatus, PrincipalCustody};

use super::enrollment::EnrollmentRetirement;
use super::now_ms;
use super::pass::PumpReport;

pub(crate) enum Cmd {
    DataVersion {
        reply: oneshot::Sender<Result<Option<u64>>>,
    },
    GetPreference {
        kind: String,
        reply: oneshot::Sender<Result<Option<StateEntry>>>,
    },
    StatesOfKind {
        kind: String,
        reply: oneshot::Sender<Result<Vec<StateEntry>>>,
    },
    PutPreference {
        kind: String,
        value: Vec<u8>,
        reply: oneshot::Sender<Result<LwwStamp>>,
    },
    RegisterContentScope {
        scope: ContentScope,
        reply: oneshot::Sender<()>,
    },
    EnqueueIntent {
        kind: String,
        scope: String,
        payload: Vec<u8>,
        drainer: IntentDrainer,
        reply: oneshot::Sender<Result<[u8; 16]>>,
    },
    SetEndpointFacts {
        facts: EndpointFacts,
        reply: oneshot::Sender<()>,
    },
    PrincipalBundleStatus {
        reply: oneshot::Sender<PrincipalBundleStatus>,
    },
    GroupCeremonyAuthority {
        reply: oneshot::Sender<Option<GroupCeremonyAuthority>>,
    },
    CachedNestCapabilities {
        reply: oneshot::Sender<Option<Vec<String>>>,
    },
    /// One caller-owned meta-table row, read
    /// ([`AccountStoreHandle::meta_get`]).
    MetaGet {
        key: String,
        reply: oneshot::Sender<Result<Option<Vec<u8>>>>,
    },
    /// …and written ([`AccountStoreHandle::meta_put`]).
    MetaPut {
        key: String,
        bytes: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    /// The store's listed fact for one scope
    /// ([`AccountStoreHandle::scope_listed`]).
    Listed {
        scope: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Which sources of a held generation's key stand for this device
    /// ([`AccountStoreHandle::unkeyed_hold`]).
    UnkeyedHold {
        reply: oneshot::Sender<Result<crate::unkeyed_hold::HoldSources>>,
    },
    RecordObservation {
        observation: Observation,
        reply: oneshot::Sender<Result<ObservationOutcome>>,
    },
    RaiseReadMarker {
        channel_id_hex: String,
        through: u64,
        reply: oneshot::Sender<Result<bool>>,
    },
    PutCustodianEndpoints {
        value: fauna_core::custodian_endpoints::CustodianEndpoints,
        reply: oneshot::Sender<Result<u64>>,
    },
    WriteContactOverlay {
        actor_id_hex: String,
        write: crate::contact_overlay_rows::OverlayWrite,
        reply: oneshot::Sender<Result<crate::contact_overlay_rows::OverlayWriteOutcome>>,
    },
    FoldContactOverlay {
        predecessor_hex: String,
        successor_hex: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// The offline-share ceremony record's write door
    /// ([`AccountStoreHandle::merge_group_shares`]), boxed because an
    /// initiated record can carry a scope's machinery root.
    MergeGroupShares {
        replica: Box<fauna_core::group_ceremony::GroupShareConfig>,
        reply: oneshot::Sender<Result<fauna_core::group_ceremony::GroupShareConfig>>,
    },
    /// The backup-destination list's write door
    /// ([`AccountStoreHandle::write_backup_destinations`]).
    WriteBackupDestinations {
        source_nest: [u8; 32],
        backup: fauna_core::data::BackupConfig,
        reply: oneshot::Sender<Result<fauna_core::backup_state::BackupState>>,
    },
    /// The destination marks' write door
    /// ([`AccountStoreHandle::merge_destination_marks`]).
    MergeDestinationMarks {
        marks: Vec<fauna_core::data::DestinationUnattestedMark>,
        reply: oneshot::Sender<Result<Vec<fauna_core::data::DestinationUnattestedMark>>>,
    },
    /// The ATProto identity custody's write door
    /// ([`AccountStoreHandle::merge_atproto_identity`]), boxed like the
    /// ceremony record: it carries the senior rotation keys.
    MergeAtprotoIdentity {
        replica: Box<fauna_core::data::AtprotoIdentityConfig>,
        reply: oneshot::Sender<Result<fauna_core::data::AtprotoIdentityConfig>>,
    },
    /// The custody ceremony state's write door
    /// ([`AccountStoreHandle::merge_custody`]), boxed like the group-share
    /// ceremony record: it carries verbatim signed envelopes.
    MergeCustody {
        replica: Box<fauna_core::custody_ceremony::CustodyConfig>,
        reply: oneshot::Sender<Result<fauna_core::custody_ceremony::CustodyConfig>>,
    },
    /// The npub confirmation's write door
    /// ([`AccountStoreHandle::confirm_nostr_npub`]).
    ConfirmNostrNpub {
        now: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// The DNS record's write door ([`AccountStoreHandle::write_dns`]),
    /// boxed like the ceremony record: it carries provider credentials.
    WriteDns {
        next: Box<fauna_core::data::DnsConfig>,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// The consent-time mint's manifest row
    /// ([`AccountStoreHandle::write_kind_manifest`]).
    WriteKindManifest {
        client_id: String,
        manifest: Box<fauna_protocol::kind_manifest::VerifiedManifest>,
        admitted_at_ms: i64,
        reply: oneshot::Sender<Result<u64>>,
    },
    /// An app credential's mint door ([`AccountStoreHandle::put_app_credential`]),
    /// boxed like the DNS record: it carries the secret.
    PutAppCredential {
        credential: Box<fauna_core::data::AtprotoAppCredential>,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// An app credential's revoke door
    /// ([`AccountStoreHandle::revoke_app_credential`]).
    RevokeAppCredential {
        credential_id: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// A followed folder's follow door ([`AccountStoreHandle::put_follow`]).
    PutFollow {
        follow: Box<fauna_core::data::FollowedFolder>,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// A followed folder's unfollow door ([`AccountStoreHandle::unfollow`]).
    Unfollow {
        home_nest_url: String,
        folder_id: i64,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// The mail-state row's write door
    /// ([`AccountStoreHandle::write_mail_state`]), boxed: it carries the
    /// MSEK and its grace window.
    WriteMailState {
        state: Box<fauna_core::mail_rows::MailStateRow>,
        now: fauna_core::data::Timestamp,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// A mail credential's put door
    /// ([`AccountStoreHandle::put_mail_credential`]), boxed: it carries the
    /// secret.
    PutMailCredential {
        credential: Box<fauna_core::data::MailCredential>,
        now: fauna_core::data::Timestamp,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// A mail credential's generation-marker door
    /// ([`AccountStoreHandle::mark_mail_credential_wrapped`]).
    MarkMailCredentialWrapped {
        credential_id: String,
        fingerprint: fauna_core::data::MsekFingerprint,
        now: fauna_core::data::Timestamp,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// A mail credential's soft-revoke door
    /// ([`AccountStoreHandle::revoke_mail_credential`]).
    RevokeMailCredential {
        credential_id: String,
        now: fauna_core::data::Timestamp,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// The succession ledger's write door
    /// ([`AccountStoreHandle::merge_succession_ledger`]), boxed because a
    /// ledger carries the whole grant log.
    MergeSuccessionLedger {
        self_actor: fauna_core::identity::ActorId,
        replica: Box<fauna_core::succession_ledger::SuccessionLedger>,
        attested: Vec<fauna_core::identity::ActorId>,
        reply: oneshot::Sender<Result<fauna_core::succession_ledger::SuccessionLedger>>,
    },
    /// Shared-folder content-key custody's write door
    /// ([`AccountStoreHandle::merge_folder_keys`]), boxed because a replica
    /// carries every set's generation history.
    MergeFolderKeys {
        replica: Box<fauna_core::data::FoldersConfig>,
        reply: oneshot::Sender<Result<fauna_core::data::FoldersConfig>>,
    },
    /// A folder-key staging's settle door
    /// ([`AccountStoreHandle::settle_folder_removal`]).
    SettleFolderRemoval {
        removal: Box<fauna_core::data::FolderPendingRemoval>,
        reply: oneshot::Sender<Result<fauna_core::data::FoldersConfig>>,
    },
    RepointSuccessionLedger {
        retired: fauna_core::identity::ActorId,
        successor: fauna_core::identity::ActorId,
        reply: oneshot::Sender<Result<bool>>,
    },
    RaiseGrantMarks {
        self_actor: fauna_core::identity::ActorId,
        predecessor: fauna_core::identity::ActorId,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// The period-key custody's write door
    /// ([`AccountStoreHandle::merge_subscriptions`]), boxed like the DNS
    /// record: it carries the period keys.
    MergeSubscriptions {
        replica: Box<fauna_core::data::SubscriptionsConfig>,
        reply: oneshot::Sender<Result<fauna_core::data::SubscriptionsConfig>>,
    },
    /// A staged removal's settle door
    /// ([`AccountStoreHandle::settle_pending_removal`]).
    SettlePendingRemoval {
        removal: Box<fauna_core::data::PendingRemoval>,
        reply: oneshot::Sender<Result<fauna_core::data::SubscriptionsConfig>>,
    },
    /// The peer-anchor cache's write door
    /// ([`AccountStoreHandle::merge_peer_anchors`]).
    MergePeerAnchors {
        replica: Box<fauna_core::data::PeerAnchors>,
        reply: oneshot::Sender<Result<fauna_core::data::PeerAnchors>>,
    },
    /// The deployment-seed custody's write door
    /// ([`AccountStoreHandle::merge_deployment_seeds`]): it carries the seeds.
    MergeDeploymentSeeds {
        replica: Vec<fauna_core::data::DeploymentSeedEntry>,
        reply: oneshot::Sender<Result<Vec<fauna_core::data::DeploymentSeedEntry>>>,
    },
    /// The deployment-seed custody's publication check
    /// ([`AccountStoreHandle::deployment_seed_published`]) — a read.
    DeploymentSeedPublished {
        nest_actor_id: [u8; 32],
        reply: oneshot::Sender<Result<bool>>,
    },
    /// The refused-change list's write door
    /// ([`AccountStoreHandle::write_refused_scheduling_changes`]).
    WriteRefusedChanges {
        write: crate::refused_change_rows::RefusedChangeWrite,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// The blessed-nests write door
    /// ([`AccountStoreHandle::set_nest_blessed`]).
    SetNestBlessed {
        nest_id: [u8; 32],
        blessed: bool,
        now: u64,
        reply: oneshot::Sender<Result<bool>>,
    },
    PutCustodiesHeld {
        value: fauna_core::custodies_held::CustodyHeld,
        reply: oneshot::Sender<Result<u64>>,
    },
    PutShareEndpoints {
        value: fauna_core::share_endpoints::ShareEndpoints,
        reply: oneshot::Sender<Result<u64>>,
    },
    /// The offline-share ceremony's two fleet-scope custody write-throughs and
    /// its group-plane adoption, boxed because a `GroupHeldRootRecord` carries
    /// the scope's machinery root and a snapshot is a whole ceremony's rows.
    PutGroupHeldRoot {
        record: Box<fauna_core::group_generation::GroupHeldRootRecord>,
        reply: oneshot::Sender<Result<u64>>,
    },
    PutGroupReceptionKey {
        record: Box<fauna_core::group_generation::GroupReceptionKeyRecord>,
        reply: oneshot::Sender<Result<u64>>,
    },
    AdoptGroupRows {
        root: Box<fauna_core::group_generation::GroupHeldRootRecord>,
        rows: Vec<fauna_core::group_ceremony::GroupPlaneRow>,
        reply: oneshot::Sender<Result<crate::group_state_plane::AdoptReport>>,
    },
    GroupScopeStates {
        scope_id: [u8; 32],
        reply: oneshot::Sender<Result<Vec<fauna_account_store::types::StateEntry>>>,
    },
    GroupRosterSnapshot {
        reply: oneshot::Sender<Result<crate::group_scope_view::GroupRosterSnapshot>>,
    },
    /// Which device principals hold the generation key at this store's
    /// resolved tip (`generation_tip::keyed_principals_at_tip`) — the
    /// keyless-posture badge's read door. `None` = no tip resolves for this
    /// observer (render nothing; fail-safe).
    KeyedPrincipals {
        reply: oneshot::Sender<Result<Option<std::collections::BTreeSet<[u8; 32]>>>>,
    },
    /// This device's own p2p participation row
    /// (`crate::p2p_participation`) — the share driver's and the devices
    /// page's read door. A local read.
    P2pParticipation {
        reply: oneshot::Sender<crate::p2p_participation::P2pParticipation>,
    },
    /// The user's own switch on this device (`device-p2p-participation-toggle`
    /// on this device's own row): rests the row. The listeners react through
    /// the two bind doors, never here.
    SetP2pParticipation {
        on: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Which `sync_devices` row this machine's grant is actually registered on
    /// ([`PrincipalSlot::grant_registration_row`]) — the This-device badge's
    /// read door. `None` = nothing enrolled for this actor yet.
    EnrolledDeviceRow {
        reply: oneshot::Sender<Option<String>>,
    },
    /// The nest's standing refusal of this machine's enrollment, if any
    /// ([`PrincipalSlot::enrollment_refusal`]) — the Devices page's
    /// `error-message` read door. `None` = nothing refused, or healed since.
    EnrollmentRefusal {
        reply: oneshot::Sender<Option<EnrollmentRefusal>>,
    },
    ReconcileNow {
        reply: oneshot::Sender<PumpReport>,
    },
    /// The explicit pass barrier ([`AccountStoreHandle::settled`]): a
    /// pass-bound no-op, answered only between passes.
    Settled {
        reply: oneshot::Sender<()>,
    },
    /// The sign-out leg: retire this machine's enrollment nest-side while the
    /// runtime still holds the writer key and the app session
    /// ([`AccountStoreHandle::shutdown_for_sign_out`]).
    RetireEnrollment {
        reply: oneshot::Sender<EnrollmentRetirement>,
    },
    /// The let-go's read ([`AccountStoreHandle::dead_generations`]).
    DeadGenerations {
        reply: oneshot::Sender<Result<Vec<crate::generation_let_go::DeadGeneration>>>,
    },
    /// The let-go itself ([`AccountStoreHandle::let_go`]).
    LetGo {
        generations: std::collections::BTreeSet<[u8; 32]>,
        reply: oneshot::Sender<Result<crate::generation_let_go::LetGoReport>>,
    },
    /// The same action's durable intent, staged BEFORE the nest deletion
    /// ([`AccountStoreHandle::stage_fleet_removal`]).
    StageFleetRemoval {
        removal: PendingFleetRemoval,
        reply: oneshot::Sender<Result<()>>,
    },
    /// …and settled on what the nest deletion came to
    /// ([`AccountStoreHandle::settle_fleet_removal`]).
    SettleFleetRemoval {
        removal: PendingFleetRemoval,
        outcome: NestDeletion,
        reply: oneshot::Sender<Result<()>>,
    },
    /// The same action's resolution half, asked BEFORE the nest deletion:
    /// which fleet members removing nest row `row` excludes
    /// ([`AccountStoreHandle::resolve_fleet_removal`]).
    ResolveFleetRemoval {
        row: String,
        claimed: Option<[u8; 32]>,
        reply: oneshot::Sender<Result<Vec<[u8; 32]>, FleetRemovalRefusal>>,
    },
    /// The devices page's read of the member-addressed door: this device's
    /// fleet id and every verified member no roster row accounts for
    /// ([`AccountStoreHandle::unaccounted_fleet_members`]).
    UnaccountedFleetMembers {
        roster: Vec<(String, Option<[u8; 32]>)>,
        reply: oneshot::Sender<Result<FleetMembersView>>,
    },
    /// The fleet ids this replica's verified fleet view excludes
    /// ([`AccountStoreHandle::removed_device_ids`]).
    RemovedDeviceIds {
        reply: oneshot::Sender<Result<std::collections::HashSet<[u8; 32]>>>,
    },
    /// The member-addressed door's one leg: resolve `member` by its fleet id
    /// and write its `Removed` row ([`AccountStoreHandle::remove_fleet_member`]).
    RemoveFleetMember {
        member: [u8; 32],
        reply: oneshot::Sender<Result<(), FleetRemovalRefusal>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

impl Cmd {
    /// Whether the store loop serves this command **inside** a pass, at the
    /// pass's next yield point, rather than parking it until the pass ends
    /// (`account-data-plane.md` § The client-side lifecycle, the pump bullet
    /// → *Commands and passes*). The test is the ruling's: a local command
    /// touches the store only through transactions of its own, holds nothing
    /// the pass loads at its start and persists at its end, borrows nothing
    /// the pass borrows exclusively, and does no network — which makes its
    /// interleaving with a pass at an await point exactly the sanctioned
    /// multi-process posture (a non-holder writing the store the holder
    /// pumps). Every variant's verdict, and the argument for it:
    ///
    /// - **The reads** (preferences, kinds, a group scope's rows, the slot's
    ///   status and grant, the cached nest facts, a caller-owned meta row) —
    ///   local: one store read or a slot read.
    /// - **`PutPreference`, `EnqueueIntent`, `RegisterContentScope`** —
    ///   local: a store write of their own (the publish is the publish
    ///   step's), or app-fed state every pass re-derives from at its start.
    /// - **`MetaGet`, `MetaPut`** — local: one meta-table read or write on a
    ///   key the CALLER owns (the share pump's transfer ledger, whose
    ///   load-at-start, persist-at-end cycle is that pump's own). A key an
    ///   account pass loads at its start and persists at its end is the
    ///   pass's, never a caller's to write — the door's contract
    ///   ([`AccountStoreHandle::meta_put`]), not something the loop checks.
    /// - **`SetEndpointFacts`** — local: every pass reads a snapshot of the
    ///   facts taken at its start, so a set during a pass borrows nothing it
    ///   holds and lands for the next one — exactly when a parked set landed.
    /// - **`RecordObservation`, `RaiseReadMarker`** — local: a read-modify-
    ///   write of one delegable (`Gen0`) entry whose read and write run with
    ///   no yield between them, so no walk page merges between the two; the
    ///   coordinate an observation resolves is whatever the walk has merged
    ///   so far — a record not yet carried answers `Unresolved`, as it does
    ///   between passes; the publish is the publish step's.
    /// - **The fleet-removal quartet** — local: resolve is a store read,
    ///   stage and settle update the slot's staged intents, persisted on
    ///   every update and never held by a pass across its run, and the
    ///   `Removed` row is a `Gen0` local write. The caller owns the order
    ///   (resolve → stage → nest deletion → settle); the pass's reconcile of
    ///   the staged intents has no yield point between its read of the slot
    ///   and its last write to it, so it reads the slot wholly before or
    ///   after a command, and its in-flight bound already covers a deletion
    ///   racing a pass (`fleet_removal` module docs).
    /// - **The member door's pair** (`UnaccountedFleetMembers`,
    ///   `RemoveFleetMember`) — local, for the quartet's reasons: the list is
    ///   the same store read as resolve, and the removal is that read followed
    ///   by the same `Gen0` local write with no yield between them.
    /// - **`RemovedDeviceIds`** — local: the same device-set store read, and
    ///   nothing written.
    /// - **`AdoptGroupRows`** — local: store transactions of its own on a
    ///   group scope, with no feed. A pass's group leg (the authority-
    ///   revocation severance) does read and write group scopes across its
    ///   awaits, but adoption is an `apply_class2` merge — monotone — so one
    ///   landing between those awaits is seen a pass later, never overwritten.
    /// - **The custody, group and share door puts** ([`Cmd::tip_sealed_kind`])
    ///   — local **while an admissible generation tip resolves**: the local
    ///   write is then store work only, and the publish is the publish
    ///   step's. With none, the door runs the first-need mint — an escrow
    ///   deposit and a publish, under the plane's mint lock, which a pass's
    ///   own tip-sealed put may be holding — so inside a pass the put is
    ///   parked instead ([`serve_local_cmd`] asks
    ///   [`AccountStatePlane::origination_mints`] first) and mints after it.
    /// - **Pass-bound:** `ReconcileNow` (a pass of its own); `Settled` (the
    ///   barrier — answered between passes by definition); `RetireEnrollment`
    ///   and `Shutdown` (they end the principal's sessions, which no pass may
    ///   be mid-flight for); `DeadGenerations` and `LetGo` (they ask the
    ///   holder and retire at the nest, and the dead read must not race a
    ///   pass's own retires and re-seals).
    pub(crate) fn is_local(&self) -> bool {
        match self {
            Cmd::DataVersion { .. }
            | Cmd::GetPreference { .. }
            | Cmd::StatesOfKind { .. }
            | Cmd::DeploymentSeedPublished { .. }
            | Cmd::PutPreference { .. }
            | Cmd::RegisterContentScope { .. }
            | Cmd::EnqueueIntent { .. }
            | Cmd::PrincipalBundleStatus { .. }
            | Cmd::GroupCeremonyAuthority { .. }
            | Cmd::CachedNestCapabilities { .. }
            | Cmd::GroupScopeStates { .. }
            | Cmd::GroupRosterSnapshot { .. }
            | Cmd::KeyedPrincipals { .. }
            | Cmd::P2pParticipation { .. }
            | Cmd::SetP2pParticipation { .. }
            | Cmd::EnrolledDeviceRow { .. }
            | Cmd::EnrollmentRefusal { .. }
            | Cmd::SetEndpointFacts { .. }
            | Cmd::RecordObservation { .. }
            | Cmd::RaiseReadMarker { .. }
            | Cmd::PutCustodianEndpoints { .. }
            | Cmd::WriteContactOverlay { .. }
            | Cmd::FoldContactOverlay { .. }
            | Cmd::MergeGroupShares { .. }
            | Cmd::MergeAtprotoIdentity { .. }
            | Cmd::MergeCustody { .. }
            | Cmd::ConfirmNostrNpub { .. }
            | Cmd::WriteBackupDestinations { .. }
            | Cmd::MergeDestinationMarks { .. }
            | Cmd::WriteDns { .. }
            | Cmd::WriteKindManifest { .. }
            | Cmd::PutAppCredential { .. }
            | Cmd::RevokeAppCredential { .. }
            | Cmd::WriteMailState { .. }
            | Cmd::PutMailCredential { .. }
            | Cmd::MarkMailCredentialWrapped { .. }
            | Cmd::RevokeMailCredential { .. }
            | Cmd::PutFollow { .. }
            | Cmd::Unfollow { .. }
            | Cmd::MergeSuccessionLedger { .. }
            | Cmd::RepointSuccessionLedger { .. }
            | Cmd::RaiseGrantMarks { .. }
            | Cmd::MergeSubscriptions { .. }
            | Cmd::SettlePendingRemoval { .. }
            | Cmd::MergeFolderKeys { .. }
            | Cmd::SettleFolderRemoval { .. }
            | Cmd::MergeDeploymentSeeds { .. }
            | Cmd::MergePeerAnchors { .. }
            | Cmd::WriteRefusedChanges { .. }
            | Cmd::SetNestBlessed { .. }
            | Cmd::PutCustodiesHeld { .. }
            | Cmd::PutShareEndpoints { .. }
            | Cmd::PutGroupHeldRoot { .. }
            | Cmd::PutGroupReceptionKey { .. }
            | Cmd::AdoptGroupRows { .. }
            | Cmd::StageFleetRemoval { .. }
            | Cmd::SettleFleetRemoval { .. }
            | Cmd::ResolveFleetRemoval { .. }
            | Cmd::UnaccountedFleetMembers { .. }
            | Cmd::RemovedDeviceIds { .. }
            | Cmd::RemoveFleetMember { .. }
            | Cmd::MetaGet { .. }
            | Cmd::MetaPut { .. }
            | Cmd::Listed { .. }
            | Cmd::UnkeyedHold { .. } => true,
            Cmd::ReconcileNow { .. }
            | Cmd::Settled { .. }
            | Cmd::RetireEnrollment { .. }
            | Cmd::DeadGenerations { .. }
            | Cmd::LetGo { .. }
            | Cmd::Shutdown { .. } => false,
        }
    }

    /// The `GenerationTip`-sealed kind a door put writes — the local
    /// commands whose local write can need the first-need mint
    /// (`Cmd::is_local`, the door-put verdict).
    pub(crate) fn tip_sealed_kind(&self) -> Option<&'static str> {
        use fauna_protocol::merge_policy as mp;
        match self {
            Cmd::PutCustodianEndpoints { .. } => Some(mp::KIND_CUSTODIAN_ENDPOINTS),
            Cmd::PutCustodiesHeld { .. } => Some(mp::KIND_CUSTODIES_HELD),
            Cmd::PutShareEndpoints { .. } => Some(mp::KIND_SHARE_ENDPOINTS),
            Cmd::PutGroupHeldRoot { .. } => Some(mp::KIND_GROUP_MACHINERY_ROOT),
            Cmd::PutGroupReceptionKey { .. } => Some(mp::KIND_GROUP_RECEPTION_KEY),
            Cmd::WriteContactOverlay { .. } | Cmd::FoldContactOverlay { .. } => {
                Some(mp::KIND_CONTACT_OVERLAY)
            }
            Cmd::MergeGroupShares { .. } => Some(mp::KIND_GROUP_SHARE_CEREMONY),
            Cmd::MergeAtprotoIdentity { .. } => Some(mp::KIND_ATPROTO_IDENTITY),
            Cmd::MergeCustody { .. } => Some(mp::KIND_CUSTODY_CEREMONY),
            Cmd::ConfirmNostrNpub { .. } => Some(mp::KIND_NOSTR_CONFIRMATION),
            Cmd::WriteBackupDestinations { .. } | Cmd::MergeDestinationMarks { .. } => {
                Some(mp::KIND_BACKUP)
            }
            Cmd::WriteDns { .. } => Some(mp::KIND_DNS),
            Cmd::WriteKindManifest { .. } => Some(mp::KIND_KIND_MANIFEST),
            Cmd::PutAppCredential { .. } | Cmd::RevokeAppCredential { .. } => {
                Some(mp::KIND_ATPROTO)
            }
            Cmd::PutFollow { .. } | Cmd::Unfollow { .. } => Some(mp::KIND_FOLLOWS),
            Cmd::WriteMailState { .. }
            | Cmd::PutMailCredential { .. }
            | Cmd::MarkMailCredentialWrapped { .. }
            | Cmd::RevokeMailCredential { .. } => Some(mp::KIND_MAIL),
            Cmd::MergeSuccessionLedger { .. }
            | Cmd::RepointSuccessionLedger { .. }
            | Cmd::RaiseGrantMarks { .. } => Some(mp::KIND_SUCCESSION_LEDGER),
            Cmd::MergeSubscriptions { .. } | Cmd::SettlePendingRemoval { .. } => {
                Some(mp::KIND_SUBSCRIPTIONS)
            }
            Cmd::MergeDeploymentSeeds { .. } => Some(mp::KIND_DEPLOYMENT_SEEDS),
            Cmd::MergePeerAnchors { .. } => Some(mp::KIND_PEER_ANCHORS),
            Cmd::WriteRefusedChanges { .. } => Some(mp::KIND_REFUSED_SCHEDULING_CHANGES),
            Cmd::SetNestBlessed { .. } => Some(mp::KIND_BLESSED_NESTS),
            Cmd::MergeFolderKeys { .. } | Cmd::SettleFolderRemoval { .. } => {
                Some(mp::KIND_FOLDER_KEYS)
            }
            _ => None,
        }
    }
}

/// Full pump-pass cycle counters — the account plane's twin of the
/// conversations receive-cycle observable (e2e convention 14: a poke plus
/// completion counters, never a cadence wait; the wire contract is
/// `fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`). `started` bumps as a pass
/// begins and `completed` as it resolves — a panic-contained pass completes
/// too — and EVERY edge counts (prologue, nudge, ticker, reconcile-now), so
/// a baseline read + [`AccountStoreHandle::reconcile_now`] + a later
/// `completed` is a latency-independent "the pump has provably run" barrier.
/// Two counters on purpose: a bare completion count cannot tell "a pass that
/// began after my trigger finished" from "a pass already in flight finished"
/// (the receive-cycle key's pigeonhole note owns the argument).
/// It also carries **the role those counters are counted under**, because
/// without it they are not interpretable. A non-holder runs no pass (§ the
/// module docs' election paragraph), so its counters sit frozen — at `(0, 0)`
/// if it never held the role — which is indistinguishable from *no runtime at
/// all* and from *elected, no pass yet* to anyone reading the numbers alone.
/// A consumer that asserted "a poked pass completes" without the role was
/// therefore asserting an election outcome nothing guarantees; measured
/// 2026-08-18, one end-to-end run, same code: one app instance reached
/// `(4, 3)` while two others sat frozen at `(0, 0)`, purely because a
/// co-located process held the lock (the account charter's § Implementation
/// status today → records the measurement).
#[derive(Debug, Default)]
pub struct PumpCycles {
    started: std::sync::atomic::AtomicU64,
    completed: std::sync::atomic::AtomicU64,
    holder: std::sync::atomic::AtomicBool,
    /// Woken at every [`Self::end`] — what
    /// [`AccountStoreHandle::pass_completed_after`] waits on.
    ended: tokio::sync::Notify,
    /// The change generation — NOT a pass counter, though it sits beside
    /// them: moved once at the end of every run of the pump (a full pass, a
    /// nudge's walk, a publish step, a seed pass) that changed an entry a
    /// read can answer, and by nothing else. A gesture's own write served
    /// inside the run is not counted, a run that changed only bookkeeping
    /// moves nothing, and a non-holder (which runs no pass) moves it only by
    /// the publish and seed runs it does run — its notice is the
    /// `data_version` floor (`account-runtime.md` § Multi-instance
    /// concurrency → *A runtime's own pump is a source of the notice
    /// too*, parts 1–3). Measured in `drive::drive_pass`.
    generation: std::sync::atomic::AtomicU64,
    /// Woken at every move of [`Self::generation`] — what
    /// [`AccountStoreHandle::changed_after`] waits on.
    changed: tokio::sync::Notify,
}

impl PumpCycles {
    /// `(started, completed)` — monotone, relaxed (counters, not fences).
    pub fn read(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (self.started.load(Relaxed), self.completed.load(Relaxed))
    }
    /// Whether this runtime currently holds the engine-singleton role — the
    /// half that makes [`read`](Self::read) interpretable.
    ///
    /// Published at every transition the worker makes (the election at
    /// assembly, and both re-try wins), so a reader sees the role the next
    /// pass would run under. Starts `false`: before the readiness barrier
    /// settles the election, "not the holder" is the honest answer and the
    /// fail-safe one — a caller waiting on a pass would rather wait than be
    /// told a pass is coming that never will.
    pub fn is_holder(&self) -> bool {
        self.holder.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Publish the role. Called by the worker at each transition; there is no
    /// un-holding — the lock is released only by the process exiting, which
    /// takes this struct with it.
    pub(crate) fn set_holder(&self, held: bool) {
        self.holder
            .store(held, std::sync::atomic::Ordering::Relaxed);
    }
    pub(crate) fn begin(&self) {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    pub(crate) fn end(&self) {
        self.completed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.ended.notify_waiters();
    }
    /// The change generation — monotone, relaxed.
    pub fn change_generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// A run of the pump changed an entry a read can answer.
    pub(crate) fn changed(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.changed.notify_waiters();
    }
    /// Wait until the change generation is past `after`; the generation then.
    pub(crate) async fn changed_after(&self, after: u64) -> u64 {
        loop {
            // Registered before the read, as in `completed_after`.
            let changed = self.changed.notified();
            let generation = self.change_generation();
            if generation > after {
                return generation;
            }
            changed.await;
        }
    }
    /// Wait until more than `after` passes have completed; the count then.
    pub(crate) async fn completed_after(&self, after: u64) -> u64 {
        loop {
            // Registered before the read, so an `end` between the two is not
            // lost: `notify_waiters` wakes every `Notified` already created.
            let ended = self.ended.notified();
            let (_, completed) = self.read();
            if completed > after {
                return completed;
            }
            ended.await;
        }
    }
}

/// The `Send + Clone` face of the runtime. All store access crosses this
/// handle; dropping every clone shuts the store thread down (the plain-quit
/// teardown), and [`Self::shutdown`] does it deterministically (sign-out).
#[derive(Clone, Debug)]
pub struct AccountStoreHandle {
    pub(super) cmd: mpsc::Sender<Cmd>,
    pub(super) nudge: mpsc::Sender<String>,
    pub(super) cycles: Arc<PumpCycles>,
    /// This process's first listings — what the first-listing gate
    /// ([`super::handle_source::first_listing_gate`]) waits on. Shared with
    /// the driver, whose bound planes record into it.
    pub(super) first_listings: Arc<crate::account_state_plane::FirstListings>,
    /// When a sign-out was first requested, as wall-clock milliseconds
    /// (`super::now_ms`) — the claim that cuts the pass in flight
    /// (`SIGN_OUT_PASS_GRACE`). Beside the command channel rather than on it,
    /// because the loop reads commands only between passes.
    pub(super) sign_out: Arc<watch::Sender<Option<u64>>>,
    /// Bumped on every [`Self::set_p2p_participation`] — the wake the share
    /// driver selects on beside its tick, so the seat drops within the pass
    /// the toggle triggers rather than a whole cadence later. A counter, not
    /// the value: the row itself is read back through the store thread.
    pub(super) p2p_participation_changed: Arc<watch::Sender<u64>>,
    /// Whether this machine's store principal has its grant on the nest —
    /// the store principal's connect gate
    /// ([`Self::subscribe_grant_registered`]). Set by the driver, shared with
    /// it, and never set back to `false`.
    pub(super) grant_registered: Arc<watch::Sender<bool>>,
    /// Who the succession-ledger seam reads and writes as — this runtime's
    /// account and its attested predecessors, both settled once in
    /// [`super::DriverConfig`] — carried so no host passes identity per call
    /// (`impl SuccessionLedgerStore for AccountStoreHandle`). `None` when the
    /// host's actor id did not decode; the seam then refuses every call.
    pub(super) ledger_identity: Option<Arc<LedgerIdentity>>,
}

/// The identity half of the succession-ledger seam: the account this runtime
/// serves and the predecessors it holds keys for (`AccountRegistry`'s
/// attested set) — the READ fold's seed and the door's signer set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerIdentity {
    /// This runtime's own account identity.
    pub self_actor: fauna_core::identity::ActorId,
    /// The attested succeeded-from identities.
    pub attested: Vec<fauna_core::identity::ActorId>,
}

/// The v1 authority seam for a group-scope ceremony
/// (`docs/goal/behavior/p2p.md` § Offline share initiation): this machine's
/// device principal signing key together with the `DeviceAuthorization`
/// carriage that proves the account root authorized it.
///
/// The two travel as ONE value because they are only meaningful together —
/// the carriage names the very key it is paired with, and splitting them
/// across two reads is what would let a rotation slip a stale witness under a
/// fresh key. Obtained from
/// [`AccountStoreHandle::group_ceremony_authority`].
pub struct GroupCeremonyAuthority {
    /// The device principal's signing key (the machine's writer key).
    pub device_key: SigningKey,
    /// The canonical encoding of the root-signed `DeviceAuthorization`
    /// carriage, ready to hand to `build_group_deliver`.
    pub device_authorization: Vec<u8>,
}

/// Channel-closed / thread-gone error text, shared by every handle call.
pub const RUNTIME_GONE: &str = "account runtime is shut down";

/// The `account_pump_cycles` e2e state key's body
/// (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`), for an app's runtime or its
/// absence — one shape for every hosting app, native and web, so the
/// cross-app contract cannot grow two spellings:
///
/// * **`runtime: false`** — the app has the leg but no assembled runtime: still
///   assembling, or the assembly failed (it is best-effort by design).
///   Counters are `0`.
/// * **`runtime: true, holder: false`** — assembled, but another co-located
///   process (a sync agent, another tab) holds the engine-singleton role. This
///   runtime **runs no pass**, so its counters are frozen *correctly* and a
///   caller must not wait on them.
/// * **`runtime: true, holder: true`** — assembled and pumping. Only here does
///   "poke, then await a completed pass" mean anything.
///
/// The key being absent is convention 11's refusal — an app with no
/// account-store leg at all — never this with zeros. A plain atomic read on
/// both halves: legal on a state-provider path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PumpCyclesView {
    pub started: u64,
    pub completed: u64,
    pub runtime: bool,
    pub holder: bool,
}

impl PumpCyclesView {
    /// Read `handle`'s counters and role, or the no-runtime shape.
    pub fn of(handle: Option<&AccountStoreHandle>) -> Self {
        let (started, completed) = handle.map(|h| h.pump_cycles()).unwrap_or((0, 0));
        Self {
            started,
            completed,
            runtime: handle.is_some(),
            holder: handle.is_some_and(|h| h.is_engine_holder()),
        }
    }
}

impl AccountStoreHandle {
    /// This runtime's pump-pass cycle counters, `(started, completed)` — a
    /// plain atomic read, never a channel round trip, so a state provider
    /// may call it on its serialization path (convention 11 corollary).
    pub fn pump_cycles(&self) -> (u64, u64) {
        self.cycles.read()
    }

    /// Whether this runtime holds the engine-singleton role — i.e.
    /// whether it is the process that pumps this store, or one of the plain
    /// reader/writers beside it.
    ///
    /// **Read this whenever you read [`pump_cycles`](Self::pump_cycles).** A
    /// non-holder runs no pass, so its counters are frozen by design and mean
    /// nothing on their own; [`PumpCycles`] owns the full argument. Same
    /// contract otherwise — a plain atomic read, safe on a state-provider
    /// serialization path (convention 11 corollary).
    pub fn is_engine_holder(&self) -> bool {
        self.cycles.is_holder()
    }

    /// Which device principals hold the generation key at this store's
    /// resolved tip — the keyless-posture badge's derivation
    /// (`ui/devices.md` § Custody facet piece 1: posture is bundle key
    /// reach, derived, never stored or asked). `Ok(None)` = no tip resolves
    /// for this observer — the caller renders NOTHING (a "holds no keys"
    /// badge must never rest on an unresolved world). A local read.
    pub async fn keyed_principals(&self) -> Result<Option<std::collections::BTreeSet<[u8; 32]>>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::KeyedPrincipals { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// **Which `sync_devices` row this machine actually enrolled on** — the
    /// This-device badge's read door (`devices.md` § This-device marker).
    ///
    /// The badge marks the row the app **enrolled on**, which is not always
    /// the `device_id` the app holds in its own local store: where a
    /// co-located sync agent provisioned by a *different* app advertises its
    /// own id, decision 2 converges the enrollment onto that agent's row
    /// (`sync-agent.md` § Credential model → the RULED 2026-08-15
    /// block), and the app's own id then names no row at all. This reports the
    /// registration latch's row half — a fact about the past, not a
    /// re-derivation — so the page never has to guess.
    ///
    /// `None` = nothing is enrolled for this actor: before the ceremony's nest
    /// legs have first succeeded, and again after a re-mint whose fresh grant
    /// encoding no longer matches the stored latch. Callers pair it with the
    /// app's own id through `fauna_devices_machine::this_device_row`, which
    /// owns the rule — the fallback is the correct answer for decision 2's
    /// cases 1 and 3, not a degraded guess. A local read; never a network call
    /// and never IPC.
    pub async fn enrolled_device_row(&self) -> Result<Option<String>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::EnrolledDeviceRow { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))
    }

    /// The nest's **standing refusal** of this machine's enrollment, if one
    /// is recorded ([`PrincipalSlot::enrollment_refusal`]) — what the Devices
    /// page paints on `error-message` (`ui/devices.md` § Errors & edge
    /// cases), with [`EnrollmentRefusal::notice`] as the sentence. A fresh
    /// read off the shared credential slot on every call, so a refusal met by
    /// the co-located agent's pump (the usual holder on a desktop) reaches an
    /// app that runs no pass of its own. `None` = nothing refused, or a later
    /// register succeeded. A local read; never a network call and never IPC.
    pub async fn enrollment_refusal(&self) -> Result<Option<EnrollmentRefusal>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::EnrollmentRefusal { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))
    }

    /// The store medium's cross-connection change counter — the
    /// notification floor (charter § Multi-instance concurrency, T9
    /// *poll-with-poke*). Moves iff **another** process/connection committed
    /// to this store — a runtime's own writes (its pump included: one
    /// backend connection per runtime) never move its own reading, so a
    /// single-instance deployment polls this forever and never fires. An
    /// app's refresh cadence compares readings and re-reads its projections
    /// on change; `None` = this medium has no counter (web — its poke is the
    /// only notice). A local read — never a network call.
    pub async fn data_version(&self) -> Result<Option<u64>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::DataVersion { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// The merged local entry for a preference `kind` (its single
    /// [`fauna_protocol::merge_policy::PREFERENCE_KEY`] item), or `None`
    /// when nothing local exists yet. A local read — never a network call.
    pub async fn get_preference(&self, kind: impl Into<String>) -> Result<Option<StateEntry>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::GetPreference {
                kind: kind.into(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Every live local entry of `kind` (one per logical key). Local read.
    pub async fn states_of_kind(&self, kind: impl Into<String>) -> Result<Vec<StateEntry>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::StatesOfKind {
                kind: kind.into(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Who holds this account's generation-key escrow — every distinct
    /// `holder_id` with a recorded `fauna.state.escrow-receipt` row, sorted.
    /// The Nests page's escrow-holder role badge reads this
    /// (`participants.md` § The participant model → Roles): derived from the
    /// receipts the holder stamped, never asserted by the nest itself. One
    /// derivation for every app hosting the account runtime (tui and linux
    /// natively, the UniFFI apps through `fauna-ffi`). A local read.
    pub async fn escrow_holders(&self) -> Result<Vec<[u8; 32]>> {
        let entries = self
            .states_of_kind(fauna_protocol::merge_policy::KIND_ESCROW_RECEIPT)
            .await?;
        Ok(escrow_holders_of(&entries))
    }

    /// The local write for an admitted preference kind
    /// (`preference_put::put_preference_local`): the plane row is durable
    /// and stamped when this answers, and it answers **inside** a pass in
    /// flight rather than behind it (a local command — `Cmd::is_local`).
    /// The network leg — the ordered own publish — is the runtime's publish
    /// step, armed by this write and run as soon as no pass is in flight
    /// (`account-client-lifecycle.md` § The client-side lifecycle, the pump
    /// bullet → wake source (4)); a publish that fails leaves an unpublished
    /// row the next pass replays, and is never this call's answer. An `Err` here is a local refusal (an unadmitted kind, a value
    /// that does not decode, a rotated writer — retry after the reassembly).
    pub async fn put_preference(
        &self,
        kind: impl Into<String>,
        value: Vec<u8>,
    ) -> Result<LwwStamp> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::PutPreference {
                kind: kind.into(),
                value,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Add a content scope to the pump's walk set (the class-1 feed walk —
    /// `content_scope_plane`). The pilot registers none; the peer
    /// content-coordinate relay tranche is this surface's first consumer.
    pub async fn register_content_scope(&self, scope: ContentScope) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::RegisterContentScope { scope, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))
    }

    /// Enqueue a durable offline intent (`outbox::enqueue_intent`
    /// through the store thread). Durable before this returns: the intent
    /// survives a restart and no store operation can drop it. It drains on
    /// the next full pump pass; an online composer that wants it out *now*
    /// follows with [`Self::reconcile_now`]. Refuses any kind that is not
    /// `OfflineQueued` (the phase-0 boundary).
    pub async fn enqueue_intent(
        &self,
        kind: impl Into<String>,
        scope: impl Into<String>,
        payload: Vec<u8>,
        drainer: IntentDrainer,
    ) -> Result<[u8; 16]> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::EnqueueIntent {
                kind: kind.into(),
                scope: scope.into(),
                payload,
                drainer,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Feed the transport facts for THIS device's
    /// `fauna.state.device-endpoints` entry — the bound listener's addresses
    /// and the relay URL the nest advertises
    /// (`NestInfoReply.iroh_relay_url`); observed wiring, never a user choice
    /// (`device_endpoints_writer` module docs own what publishes when).
    /// Takes effect on the next full pump pass — pair with
    /// [`Self::reconcile_now`] for an immediate publish.
    pub async fn set_endpoint_facts(&self, facts: EndpointFacts) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::SetEndpointFacts { facts, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))
    }

    /// Write the owner-side `fauna.state.custodian-endpoints` registry row
    /// for one custody (ceremony step 3 — every fleet replica learns
    /// whom to serve and how to dial it). Through the fleet plane's REAL
    /// generation writer door on the store thread; a no-tip refusal surfaces as
    /// `Err` and the ceremony driver keeps the write owed
    /// (`custody_rows` owns the row mechanics).
    pub async fn put_custodian_endpoints(
        &self,
        value: fauna_core::custodian_endpoints::CustodianEndpoints,
    ) -> Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::PutCustodianEndpoints { value, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Write the private contact overlay (`fauna.state.contact-overlay`) on
    /// the person `actor_id_hex` — the private section's Save
    /// (`contacts.md` § The private overlay). Only the registers `write`
    /// names are re-stamped; the read-modify-write runs whole on the store
    /// thread. A refusal at the writer door while no generation tip resolves
    /// surfaces as `Err` (the save error); a label-cap refusal as
    /// [`OverlayWriteOutcome::Refused`]
    /// (`contact_overlay_rows` owns the mechanics).
    ///
    /// **It crosses the read gate** ([`super::handle_source::read_gate`]):
    /// every register the write names is re-stamped now, from the store
    /// thread's read of the overlay, so on a replica that has never listed
    /// the fleet scope, or that holds rows under a generation it may still be
    /// keyed for (the unkeyed hold — the kind is tip-sealed), a note typed
    /// over the empty form would outrank the account's real one — the save
    /// is refused ([`super::handle_source::ScopeNotReady`]) and nothing is
    /// put.
    /// The `ContactsCache` projection's load ([`Self::contact_overlays`])
    /// stays ungated: it only renders what the store holds.
    ///
    /// [`OverlayWriteOutcome::Refused`]: crate::contact_overlay_rows::OverlayWriteOutcome::Refused
    pub async fn write_contact_overlay(
        &self,
        actor_id_hex: &str,
        write: crate::contact_overlay_rows::OverlayWrite,
    ) -> Result<crate::contact_overlay_rows::OverlayWriteOutcome> {
        super::handle_source::read_gate(self, fauna_protocol::merge_policy::KIND_CONTACT_OVERLAY)
            .await?;
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::WriteContactOverlay {
                actor_id_hex: actor_id_hex.to_string(),
                write,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Fold the contact overlay on `predecessor_hex` forward onto its
    /// **witness-verified** successor `successor_hex` — the statement
    /// consumer's half of `contacts.md` § The private overlay → *When a
    /// person's identity succeeds* (`contact_overlay_rows::fold_contact_overlay`
    /// owns the mechanics). Trusts its caller: never hand it an unverified
    /// claim. Whether anything was written.
    pub async fn fold_contact_overlay(
        &self,
        predecessor_hex: &str,
        successor_hex: &str,
    ) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::FoldContactOverlay {
                predecessor_hex: predecessor_hex.to_string(),
                successor_hex: successor_hex.to_string(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Every non-empty contact overlay this store holds, keyed by actor id —
    /// the `ContactsCache` projection's load. A local read.
    pub async fn contact_overlays(
        &self,
    ) -> Result<std::collections::BTreeMap<String, fauna_core::contact_overlay::ContactOverlay>>
    {
        Ok(crate::contact_overlay_rows::overlays_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_CONTACT_OVERLAY)
                .await?,
        ))
    }

    /// This account's DNS management record (`fauna.state.dns`) — the default
    /// (no credentials, nothing managed) one when none is stored. A local
    /// read; `dns_rows` owns the mechanics.
    ///
    /// **It crosses the read gate** ([`super::handle_source::read_gate`]):
    /// the record is one whole-record latest-wins row and every write of it
    /// is a replace computed from this read, so on a replica that has never
    /// listed the fleet scope, or that holds rows under a generation it may
    /// still be keyed for (the unkeyed hold), the read is refused
    /// ([`super::handle_source::ScopeNotReady`]) rather than answering the
    /// default record as the account's.
    pub async fn dns(&self) -> Result<fauna_core::data::DnsConfig> {
        super::handle_source::read_gate(self, fauna_protocol::merge_policy::KIND_DNS).await?;
        crate::dns_rows::dns_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_DNS)
                .await?,
        )
    }

    /// Replace this account's DNS management record (`fauna.state.dns`) with
    /// `next` — whole-record latest-wins, stamped on the store thread.
    /// Whether anything was written (`false` when the stored record already
    /// equals `next`). A refusal at the writer door while no generation tip
    /// resolves surfaces as `Err` (`dns_rows` owns the mechanics).
    pub async fn write_dns(&self, next: fauna_core::data::DnsConfig) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::WriteDns {
                next: Box::new(next),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Publish the consent's verified kind manifest as the account's
    /// `fauna.state.kind-manifest` row for `client_id`, stamped on the store
    /// thread — step (1) of the consent-time mint (`third-party-kinds.md`
    /// § The record doors; `kind_manifest_rows` owns the mechanics). A
    /// refusal at the writer door while no generation tip resolves surfaces
    /// as `Err`.
    pub async fn write_kind_manifest(
        &self,
        client_id: &str,
        manifest: &fauna_protocol::kind_manifest::VerifiedManifest,
        admitted_at_ms: i64,
    ) -> Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::WriteKindManifest {
                client_id: client_id.to_string(),
                manifest: Box::new(manifest.clone()),
                admitted_at_ms,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// The overlay this account's verified `fauna.state.kind-manifest` rows
    /// admit — a local read, each row re-verified against its own
    /// `client_id`'s host (`kind_manifest_rows::admitted_kinds_of`).
    pub async fn admitted_kinds(&self) -> Result<crate::kind_manifest_rows::AdmittedOverlay> {
        Ok(crate::kind_manifest_rows::admitted_kinds_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_KIND_MANIFEST)
                .await?,
        ))
    }

    /// This account's minted ATProto app credentials (`fauna.state.atproto`)
    /// — the read fold over the per-credential rows, oldest first; empty when
    /// none is stored. A local read; `atproto_rows` owns the mechanics.
    pub async fn atproto(&self) -> Result<fauna_core::data::AtprotoConfig> {
        crate::atproto_rows::atproto_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_ATPROTO)
                .await?,
        )
    }

    /// Put one app credential at its own `credential_id`
    /// (`fauna.state.atproto`) — latest-wins per credential, stamped on the
    /// store thread. Whether anything was written (`false` when the stored row
    /// already equals `credential`). A refusal at the writer door while no
    /// generation tip resolves surfaces as `Err` (`atproto_rows` owns the
    /// mechanics).
    pub async fn put_app_credential(
        &self,
        credential: fauna_core::data::AtprotoAppCredential,
    ) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::PutAppCredential {
                credential: Box::new(credential),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Revoke the app credential at `credential_id` (`fauna.state.atproto`) —
    /// a stamped tombstone. Whether anything was written (`false` when no live
    /// row is stored there); the writer door's refusals surface as `Err`.
    pub async fn revoke_app_credential(&self, credential_id: String) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::RevokeAppCredential {
                credential_id,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's followed public folders (`fauna.state.follows`) — the
    /// read fold over the per-folder rows, in canonical order; empty when none
    /// is stored. A local read; `follows_rows` owns the mechanics.
    pub async fn follows(&self) -> Result<fauna_core::data::FollowsConfig> {
        crate::follows_rows::follows_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_FOLLOWS)
                .await?,
        )
    }

    /// Follow a public folder, or refresh a follow in place
    /// (`fauna.state.follows`) — latest-wins per folder, stamped on the store
    /// thread. Whether anything was written (`false` when the stored row
    /// already equals `follow`). A refusal at the writer door while no
    /// generation tip resolves surfaces as `Err` (`follows_rows` owns the
    /// mechanics).
    pub async fn put_follow(&self, follow: fauna_core::data::FollowedFolder) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::PutFollow {
                follow: Box::new(follow),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Unfollow the folder at `(home_nest_url, folder_id)`
    /// (`fauna.state.follows`) — a stamped tombstone. Whether anything was
    /// written (`false` when no live row is stored there, which is success);
    /// the writer door's refusals surface as `Err`.
    pub async fn unfollow(&self, home_nest_url: String, folder_id: i64) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::Unfollow {
                home_nest_url,
                folder_id,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's mail custody (`fauna.state.mail`) — the READ fold over
    /// the state row and the credential rows (revoked rows hidden, burned
    /// rows shown); the default (mail never enabled) when none rests. A local
    /// read; `mail_rows` owns the mechanics.
    pub async fn mail(&self) -> Result<fauna_core::data::MailConfig> {
        crate::mail_rows::mail_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_MAIL)
                .await?,
        )
    }

    /// This account's mail rows, decoded and joined per key — the fold's
    /// working set, revoked credentials INCLUDED: what the spent-id rule
    /// (`MailRows::spent_credential_ids`) and the derived owed set
    /// (`MailRows::owed_rewrap`) read. A local read.
    pub async fn mail_rows(&self) -> Result<fauna_core::mail_rows::MailRows> {
        crate::mail_rows::mail_rows_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_MAIL)
                .await?,
        )
    }

    /// Write this account's mail-state row (`fauna.state.mail`, key `self`):
    /// `state`'s content stamped strictly above the stored row and joined with
    /// it on the store thread. Whether anything was written (`false` when the
    /// stored row already holds this content); the writer door's refusals
    /// surface as `Err` (`mail_rows` owns the mechanics).
    pub async fn write_mail_state(
        &self,
        state: fauna_core::mail_rows::MailStateRow,
    ) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::WriteMailState {
                state: Box::new(state),
                now: fauna_core::data::Timestamp::now(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Put one mail credential at `credential/<credential_id>` — a
    /// read-join-put, so a marker the store holds is never undone. Whether
    /// anything was written; the writer door's refusals surface as `Err`.
    pub async fn put_mail_credential(
        &self,
        credential: fauna_core::data::MailCredential,
    ) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::PutMailCredential {
                credential: Box::new(credential),
                now: fauna_core::data::Timestamp::now(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Record the MSEK generation `credential_id`'s nest-side blobs are
    /// wrapped under (*The generation marker*). Whether anything was written
    /// (`false` for an absent, marked or already-current row).
    pub async fn mark_mail_credential_wrapped(
        &self,
        credential_id: String,
        fingerprint: fauna_core::data::MsekFingerprint,
    ) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MarkMailCredentialWrapped {
                credential_id,
                fingerprint,
                now: fauna_core::data::Timestamp::now(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Soft-revoke the mail credential at `credential_id` — the monotone
    /// marker, the generation cleared and the secret emptied together; the
    /// id stays spent. Whether anything was written (`false` for an absent or
    /// already-revoked row).
    pub async fn revoke_mail_credential(&self, credential_id: String) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::RevokeMailCredential {
                credential_id,
                now: fauna_core::data::Timestamp::now(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's subscription-tier period-key custody
    /// (`fauna.state.subscriptions`) — the read fold over the per-period and
    /// per-removal rows through the shipped rule; empty when none is stored.
    /// A local read; `subscription_rows` owns the mechanics.
    pub async fn subscriptions(&self) -> Result<fauna_core::data::SubscriptionsConfig> {
        crate::subscription_rows::subscriptions_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_SUBSCRIPTIONS)
                .await?,
        )
    }

    /// Join `replica` into this account's period-key custody
    /// (`fauna.state.subscriptions`) and answer the custody as it now stands.
    /// The read-join-put runs whole on the store thread and puts only the rows
    /// the join moved; nothing is ever deleted, so a stale replica drops
    /// nothing. A refusal at the writer door while no generation tip resolves
    /// surfaces as `Err` (`subscription_rows` owns the mechanics).
    pub async fn merge_subscriptions(
        &self,
        replica: fauna_core::data::SubscriptionsConfig,
    ) -> Result<fauna_core::data::SubscriptionsConfig> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MergeSubscriptions {
                replica: Box::new(replica),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Settle one staged subscriber removal (`fauna.state.subscriptions`):
    /// its fresh period is written as a period row, then the removal row is
    /// marked settled and leaves the fold on every replica. Answers the
    /// custody as it now stands; the writer door's refusals surface as `Err`.
    pub async fn settle_pending_removal(
        &self,
        removal: fauna_core::data::PendingRemoval,
    ) -> Result<fauna_core::data::SubscriptionsConfig> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::SettlePendingRemoval {
                removal: Box::new(removal),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's deployment-seed custody map
    /// (`fauna.state.deployment-seeds`) — the read fold over the per-box rows
    /// through the shipped rule; empty when none is stored. A local read;
    /// `deployment_seed_rows` owns the mechanics.
    pub async fn deployment_seeds(&self) -> Result<Vec<fauna_core::data::DeploymentSeedEntry>> {
        crate::deployment_seed_rows::deployment_seeds_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_DEPLOYMENT_SEEDS)
                .await?,
        )
    }

    /// Join `replica` into this account's deployment-seed custody
    /// (`fauna.state.deployment-seeds`) and answer the map as it now stands —
    /// a capture is a merge of the new box's entry, a rotation's supersession
    /// mark a merge of the marked one. The read-join-put runs whole on the
    /// store thread and puts only the rows the join moved; nothing is ever
    /// deleted, so a stale replica drops nothing. An entry the plane would
    /// refuse, and the writer door's refusal while no generation tip
    /// resolves, surface as `Err` (`deployment_seed_rows` owns the mechanics).
    pub async fn merge_deployment_seeds(
        &self,
        replica: Vec<fauna_core::data::DeploymentSeedEntry>,
    ) -> Result<Vec<fauna_core::data::DeploymentSeedEntry>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MergeDeploymentSeeds { replica, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Whether the deployment-seed custody row for `nest_actor_id` rests and
    /// this device owes the bound nest no write of it — the plane rotation
    /// drive's gate before its dispatch. A local read;
    /// `deployment_seed_rows::deployment_seed_published` owns the mechanics.
    pub async fn deployment_seed_published(&self, nest_actor_id: [u8; 32]) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::DeploymentSeedPublished {
                nest_actor_id,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's peer anchors (`fauna.state.peer-anchors`) — the
    /// succession witness's chain heads and harvested domains for other
    /// identities, the read fold over the per-actor rows through the shipped
    /// rule, ceiling included; empty when none is stored. A local read;
    /// `peer_anchor_rows` owns the mechanics.
    pub async fn peer_anchors(&self) -> Result<fauna_core::data::PeerAnchors> {
        crate::peer_anchor_rows::peer_anchors_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_PEER_ANCHORS)
                .await?,
        )
    }

    /// Join `replica` into this account's peer anchors
    /// (`fauna.state.peer-anchors`) and answer them as they now stand — a
    /// harvest's seed, a walk's advance and an outrun mark are all merges of
    /// the edited anchors. The read-join-put runs whole on the store thread
    /// and puts only the rows the join moved and the ceiling keeps; nothing
    /// is ever deleted, so a behind replica rewinds nothing. An entry the
    /// plane would refuse, and the writer door's refusal while no generation
    /// tip resolves, surface as `Err` (`peer_anchor_rows` owns the mechanics).
    pub async fn merge_peer_anchors(
        &self,
        replica: fauna_core::data::PeerAnchors,
    ) -> Result<fauna_core::data::PeerAnchors> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MergePeerAnchors {
                replica: Box::new(replica),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's per-nest blessing verdicts
    /// (`fauna.state.blessed-nests`) — the read fold over the per-nest rows
    /// through the shipped rule, sorted by `nest_id`, un-blessed entries
    /// included; empty when none is stored. A local read;
    /// `blessed_nest_rows` owns the mechanics.
    pub async fn blessed_nests(&self) -> Result<Vec<fauna_core::data::BlessedNest>> {
        crate::blessed_nest_rows::blessed_nests_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_BLESSED_NESTS)
                .await?,
        )
    }

    /// Whether `nest_id` is blessed (`fauna.state.blessed-nests`) — an absent
    /// row is not. A local read of the kind; `blessed_nest_rows` owns the
    /// mechanics.
    pub async fn nest_blessed(&self, nest_id: [u8; 32]) -> Result<bool> {
        let key = fauna_core::blessed_nest_rows::blessed_nest_key(&nest_id);
        let entries: Vec<StateEntry> = self
            .states_of_kind(fauna_protocol::merge_policy::KIND_BLESSED_NESTS)
            .await?
            .into_iter()
            .filter(|e| e.key == key)
            .collect();
        Ok(crate::blessed_nest_rows::blessed_nests_of(&entries)?
            .iter()
            .any(|b| b.blessed))
    }

    /// Record the user's blessing verdict for `nest_id` at `now`
    /// (`fauna.state.blessed-nests`) — the stamp never at or behind the
    /// stored verdict, so a toggle always supersedes the state it was made
    /// against. The read-stamp-put runs whole on the store thread; whether a
    /// put happened (a re-assert of the current verdict writes nothing). The
    /// writer door's refusal while no generation tip resolves surfaces as
    /// `Err` (`blessed_nest_rows` owns the mechanics).
    pub async fn set_nest_blessed(
        &self,
        nest_id: [u8; 32],
        blessed: bool,
        now: u64,
    ) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::SetNestBlessed {
                nest_id,
                blessed,
                now,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's refused inbound scheduling changes
    /// (`fauna.state.refused-scheduling-changes`) — the whole list, dismissed
    /// rows included (a surface reads `RefusedSchedulingChanges::open`);
    /// empty when none is stored. A local read; `refused_change_rows` owns the
    /// mechanics.
    pub async fn refused_scheduling_changes(
        &self,
    ) -> Result<fauna_core::data::RefusedSchedulingChanges> {
        crate::refused_change_rows::refused_scheduling_changes_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_REFUSED_SCHEDULING_CHANGES)
                .await?,
        )
    }

    /// Apply one write to this account's refused inbound scheduling changes
    /// (`fauna.state.refused-scheduling-changes`) — a refusal recorded, a row
    /// dismissed, or a held list joined in — answering whether it changed
    /// anything. The read-modify-put runs whole on the store thread and puts
    /// only when the row's bytes moved; the writer door's refusal while no
    /// generation tip resolves surfaces as `Err` (`refused_change_rows` owns
    /// the mechanics).
    pub async fn write_refused_scheduling_changes(
        &self,
        write: crate::refused_change_rows::RefusedChangeWrite,
    ) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::WriteRefusedChanges { write, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Join `replica` into this account's offline-share ceremony record
    /// (`fauna.state.group-share-ceremony`) and answer the record as it now
    /// stands — the ceremony seat's flush (`p2p.md` § Offline share
    /// initiation). The read-join-put runs whole on the store thread and puts
    /// only when the join moved the row; a refusal at the writer door while
    /// no generation tip resolves surfaces as `Err` (`group_share_rows` owns
    /// the mechanics).
    pub async fn merge_group_shares(
        &self,
        replica: fauna_core::group_ceremony::GroupShareConfig,
    ) -> Result<fauna_core::group_ceremony::GroupShareConfig> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MergeGroupShares {
                replica: Box::new(replica),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's offline-share ceremony record — empty when none rests
    /// yet. A local read: the store is durable and answers with no nest,
    /// which is the co-present ceremony's whole case.
    pub async fn group_shares(&self) -> Result<fauna_core::group_ceremony::GroupShareConfig> {
        crate::group_share_rows::group_shares_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_GROUP_SHARE_CEREMONY)
                .await?,
        )
    }

    /// Replace `source_nest`'s backup-destination list
    /// (`fauna.state.backup`'s `destinations/<source nest>` row — the box
    /// the caller's connection is bound to, `backup-destinations.md`
    /// § *Destination data model*) and answer that box's state as it now
    /// stands. The read-stamp-put runs whole on the store thread; a
    /// list over the row's bounds is refused before anything is sealed, and
    /// a refusal at the writer door while no generation tip resolves surfaces
    /// as `Err` (`backup_rows` owns the mechanics).
    pub async fn write_backup_destinations(
        &self,
        source_nest: [u8; 32],
        backup: fauna_core::data::BackupConfig,
    ) -> Result<fauna_core::backup_state::BackupState> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::WriteBackupDestinations {
                source_nest,
                backup,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Join `marks` into this account's destination-mark rows (raising a
    /// mark, or recording a verdict on one) and answer every mark of the
    /// account as it now stands. Puts only the rows the join moved.
    pub async fn merge_destination_marks(
        &self,
        marks: Vec<fauna_core::data::DestinationUnattestedMark>,
    ) -> Result<Vec<fauna_core::data::DestinationUnattestedMark>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MergeDestinationMarks { marks, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// `source_nest`'s backup-destination state — that box's destination
    /// list pruned of every removed destination, and every mark of the
    /// account. A local read; the empty state when the box keeps no list.
    pub async fn backup_state(
        &self,
        source_nest: [u8; 32],
    ) -> Result<fauna_core::backup_state::BackupState> {
        crate::backup_rows::backup_state_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_BACKUP)
                .await?,
            &source_nest,
        )
    }

    /// Every box's destination-list row the account holds — the all-boxes
    /// read (the succession aftermath's mark raise marks every destination
    /// any of them lists; the rotated-box re-file finds its predecessor's
    /// among them). A local read.
    pub async fn backup_destination_lists(
        &self,
    ) -> Result<Vec<fauna_core::backup_state::BackupDestinationsRow>> {
        crate::backup_rows::destination_lists_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_BACKUP)
                .await?,
        )
    }

    /// Join `replica` into this account's succession ledger
    /// (`fauna.state.succession-ledger`) and answer the ledger as
    /// `self_actor` now reads it. `attested` is the predecessors whose keys
    /// this runtime holds — with `self_actor`, the signer set the door
    /// admits events from. The read-join-put runs whole on the store thread
    /// and puts only rows the join moved; the door's refusals and the writer
    /// door's no-tip refusal surface as `Err` (`succession_ledger_rows` owns
    /// the mechanics).
    pub async fn merge_succession_ledger(
        &self,
        self_actor: fauna_core::identity::ActorId,
        replica: fauna_core::succession_ledger::SuccessionLedger,
        attested: Vec<fauna_core::identity::ActorId>,
    ) -> Result<fauna_core::succession_ledger::SuccessionLedger> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MergeSuccessionLedger {
                self_actor,
                replica: Box::new(replica),
                attested,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// The succession write: re-point the ledger's chain from the ATTESTED
    /// `retired` identity to `successor` (this runtime's own). `Ok(false)`
    /// when the chain already says so.
    pub async fn repoint_succession_ledger(
        &self,
        retired: fauna_core::identity::ActorId,
        successor: fauna_core::identity::ActorId,
    ) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::RepointSuccessionLedger {
                retired,
                successor,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Raise an `Open` grant mark keyed on `predecessor` for every grant live
    /// in the ledger as `self_actor` reads it whose latest event
    /// `predecessor` signed (`succession_ledger_rows::raise_grant_marks` says
    /// why the provenance filter). Idempotent: `Ok(false)` when every mark
    /// already rests.
    pub async fn raise_grant_marks(
        &self,
        self_actor: fauna_core::identity::ActorId,
        predecessor: fauna_core::identity::ActorId,
    ) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::RaiseGrantMarks {
                self_actor,
                predecessor,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's succession ledger as `self_actor` reads it — its own
    /// identity alone when no row rests yet. A local read.
    pub async fn succession_ledger(
        &self,
        self_actor: fauna_core::identity::ActorId,
    ) -> Result<fauna_core::succession_ledger::SuccessionLedger> {
        crate::succession_ledger_rows::succession_ledger_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_SUCCESSION_LEDGER)
                .await?,
            self_actor,
        )
    }

    /// Join `replica` into this account's shared-folder content-key custody
    /// (`fauna.state.folder-keys`) and answer the custody as it now reads.
    /// The read-join-put runs whole on the store thread and puts only rows the
    /// join moved — a write adds and advances, never drops; the door's refusal
    /// and the writer door's no-tip refusal surface as `Err`
    /// (`folder_key_rows` owns the mechanics).
    pub async fn merge_folder_keys(
        &self,
        replica: fauna_core::data::FoldersConfig,
    ) -> Result<fauna_core::data::FoldersConfig> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MergeFolderKeys {
                replica: Box::new(replica),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Settle one staged folder-key removal (`fauna.state.folder-keys`): its
    /// fresh generation written as a generation row of the set it rotates,
    /// the staging marked settled so it leaves the fold on every replica —
    /// the plane twin of the blob's `clear_pending_removal`. Idempotent;
    /// answers the custody as it now reads (`folder_key_rows` owns the
    /// mechanics).
    pub async fn settle_folder_removal(
        &self,
        removal: fauna_core::data::FolderPendingRemoval,
    ) -> Result<fauna_core::data::FoldersConfig> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::SettleFolderRemoval {
                removal: Box::new(removal),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's shared-folder content-key custody — empty when no row
    /// rests yet. A local read.
    pub async fn folder_keys(&self) -> Result<fauna_core::data::FoldersConfig> {
        crate::folder_key_rows::folder_keys_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_FOLDER_KEYS)
                .await?,
        )
    }

    /// Join `replica` into this account's ATProto identity custody
    /// (`fauna.state.atproto-identity`: the senior rotation keys, the
    /// tombstone consents, the contest intents, the nest-named DIDs) and
    /// answer the custody as it now stands. The read-join-put runs whole on
    /// the store thread and puts only the rows the join moved; nothing is
    /// ever removed; a refusal at the writer door while no generation tip
    /// resolves surfaces as `Err` (`atproto_identity_rows` owns the
    /// mechanics).
    pub async fn merge_atproto_identity(
        &self,
        replica: fauna_core::data::AtprotoIdentityConfig,
    ) -> Result<fauna_core::data::AtprotoIdentityConfig> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MergeAtprotoIdentity {
                replica: Box::new(replica),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Join `replica` into this account's custody-ceremony state
    /// (`fauna.state.custody-ceremony`: every ceremony it runs, as owner and
    /// as host) and answer the state as it now stands. The read-join-put runs
    /// whole on the store thread and puts only the rows the join moved;
    /// nothing is ever removed; a refusal at the writer door while no
    /// generation tip resolves surfaces as `Err` (`custody_ceremony_rows`
    /// owns the mechanics).
    pub async fn merge_custody(
        &self,
        replica: fauna_core::custody_ceremony::CustodyConfig,
    ) -> Result<fauna_core::custody_ceremony::CustodyConfig> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MergeCustody {
                replica: Box::new(replica),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// When the owner last confirmed the linked npub
    /// (`fauna.state.nostr-confirmation`), epoch seconds — `None` when they
    /// never have. A local read: the store is durable and answers with no
    /// nest.
    pub async fn npub_confirmed_at(&self) -> Result<Option<i64>> {
        Ok(crate::nostr_confirmation_rows::nostr_confirmation_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_NOSTR_CONFIRMATION)
                .await?,
        )?
        .confirmed_at)
    }

    /// Record the owner's "yes, that's my npub" confirmation at `now` (epoch
    /// seconds) — joined as the max with the stored stamp, so a later one
    /// another device wrote is never lowered. Whether anything was written.
    /// A refusal at the writer door while no generation tip resolves surfaces
    /// as `Err` (`nostr_confirmation_rows` owns the mechanics).
    pub async fn confirm_nostr_npub(&self, now: i64) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::ConfirmNostrNpub { now, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This account's custody-ceremony state — empty when none rests yet. A
    /// local read: the store is durable and answers with no nest.
    pub async fn custody(&self) -> Result<fauna_core::custody_ceremony::CustodyConfig> {
        crate::custody_ceremony_rows::custody_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_CUSTODY_CEREMONY)
                .await?,
        )
    }

    /// This account's ATProto identity custody — empty when none rests yet.
    /// A local read: the store is durable and answers with no nest.
    pub async fn atproto_identity(&self) -> Result<fauna_core::data::AtprotoIdentityConfig> {
        crate::atproto_identity_rows::atproto_identity_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_ATPROTO_IDENTITY)
                .await?,
        )
    }

    /// Write this account's custodian-side `fauna.state.custodies-held`
    /// registry row for one held custody (the witness + owner candidates +
    /// budget the serving legs read). Same door discipline as
    /// [`Self::put_custodian_endpoints`].
    pub async fn put_custodies_held(
        &self,
        value: fauna_core::custodies_held::CustodyHeld,
    ) -> Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::PutCustodiesHeld { value, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Write one held group machinery root (`fauna.state.group-machinery-root`)
    /// through this account's **fleet-scope** plane — the offline-share
    /// ceremony's custody write-through, on both sides
    /// (`BegunGroupShare::held_root_row` for the initiator,
    /// `AdmittedGroupShare::held_root_row` for the joiner).
    ///
    /// The ceremony's monotone `root_row_written` marker is the CALLER's to
    /// set, and only after this returns `Ok` — that ordering is the whole
    /// record-then-act law here: a marker set before the row exists would tell
    /// a resuming driver the scope is readable when its root is gone.
    pub async fn put_group_held_root(
        &self,
        record: fauna_core::group_generation::GroupHeldRootRecord,
    ) -> Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::PutGroupHeldRoot {
                record: Box::new(record),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Write one group-reception keypair record
    /// (`fauna.state.group-reception-key`) through the fleet-scope plane — the
    /// keypair a recipient mints when consenting, whose secret half opens the
    /// admission bundle.
    ///
    /// **Write this BEFORE building the accept.** The published half rides the
    /// accept envelope and the initiator seals the admission bundle to it; a
    /// crash between posting the accept and persisting the keypair would leave
    /// a delivery nothing on this account can ever open. Old rows are retained
    /// by the kind's own policy — old generations' wraps still target old keys.
    pub async fn put_group_reception_key(
        &self,
        record: fauna_core::group_generation::GroupReceptionKeyRecord,
    ) -> Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::PutGroupReceptionKey {
                record: Box::new(record),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Every group-reception keypair this account holds, **newest first** —
    /// the read half of [`Self::put_group_reception_key`].
    ///
    /// Until this existed the kind was write-only from outside the runtime:
    /// the ceremony driver minted a keypair, wrote it, and kept the record in
    /// hand for the rest of that flow, so nothing ever needed to read one
    /// back. A community room does — its generation wraps are addressed to
    /// these keys and are opened whenever the room is next read, in a later
    /// process than the one that was seated
    /// (`conversation-rooms.md` § The three classes → *Community*).
    ///
    /// `first()` is the account's current wrap target — what a seating hands
    /// the room. The rest are retained because a generation minted before the
    /// account's last rotation addressed its wrap to the key of that moment,
    /// so a reader that kept only the newest could not open the room's own
    /// history. See
    /// [`crate::group_state_plane::reception_keys_from_rows`] for the ordering
    /// rule and for why a corrupt row is skipped rather than fatal.
    pub async fn group_reception_keys(
        &self,
    ) -> Result<Vec<fauna_core::group_generation::GroupReceptionKeyRecord>> {
        let rows = self
            .states_of_kind(fauna_protocol::merge_policy::KIND_GROUP_RECEPTION_KEY)
            .await?;
        Ok(crate::group_state_plane::reception_keys_from_rows(rows))
    }

    /// Adopt a ceremony's machinery snapshot into this account's own group
    /// plane for the scope `root` names — birth, roster entries and the first
    /// mint, through `apply_class2`'s ordinary first-contact strictness.
    ///
    /// The scope comes from the held-root record rather than a separate
    /// argument on purpose: the root is what the plane's entries seal under,
    /// so a caller cannot pair one scope's rows with another's root.
    ///
    /// A non-zero [`crate::group_state_plane::AdoptReport::refused`] is never
    /// benign — the ceremony verifier already refused a snapshot that does not
    /// admit us, so a refusal here means a forged or corrupted row — but the
    /// pass is skip-not-abort, so the rest of the scope still lands.
    pub async fn adopt_group_rows(
        &self,
        root: fauna_core::group_generation::GroupHeldRootRecord,
        rows: Vec<fauna_core::group_ceremony::GroupPlaneRow>,
    ) -> Result<crate::group_state_plane::AdoptReport> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::AdoptGroupRows {
                root: Box::new(root),
                rows,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This replica's opened machinery rows for one group scope — the app
    /// surfaces' listing read (`account-data-plane.md` § Implementation status
    /// today: "the listing read is `AccountStore::group_scope_states`").
    ///
    /// A scope whose birth row is present here EXISTS on this device; that is
    /// the fact a folders page paints, and it is deliberately a store read
    /// rather than a config read — the ceremony record says what happened, the
    /// plane says what landed, and only the second can list a set.
    pub async fn group_scope_states(
        &self,
        scope_id: [u8; 32],
    ) -> Result<Vec<fauna_account_store::types::StateEntry>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::GroupScopeStates { scope_id, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Every group scope this replica holds, folded into the peer witness
    /// door's evaluator state ([`crate::group_scope_view::GroupRosterSnapshot`])
    /// — the share plane's pump reads it once per pass and swaps it in, so the
    /// admit path never waits on the store. A local read — never a network
    /// call.
    pub async fn group_roster_snapshot(
        &self,
    ) -> Result<crate::group_scope_view::GroupRosterSnapshot> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::GroupRosterSnapshot { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// What the T10 principal bundle carries in this store's credential slot
    /// (`principal_bundle` owns the carriage): the verified
    /// enrollment grant when one is present, the retained generation-key
    /// count, and whether the backup key is persisted. A local read — never
    /// a network call.
    pub async fn principal_bundle_status(&self) -> Result<PrincipalBundleStatus> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::PrincipalBundleStatus { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))
    }

    /// This machine's **group-ceremony authority**: the device principal's
    /// signing key paired with the `DeviceAuthorization` carriage that proves
    /// the account root authorized it — the v1 authority seam
    /// `fauna_client_capabilities::group_ceremony::build_group_deliver` takes
    /// (roster entries and generation mints are authority-device-signed with
    /// the chain carried inline).
    ///
    /// `Ok(None)` = this machine is not enrolled (no ceremony has run here), so
    /// it cannot *initiate* a share. That is a quiet, self-healing absence —
    /// only a signed-in enrollment ceremony changes it — never an error.
    ///
    /// Resolved on the store thread on purpose: the key and the witness that
    /// authorizes it must come from the same assembly, or a rotation could pair
    /// a fresh key with a stale carriage and mint entries nothing verifies.
    /// A local read — never a network call.
    pub async fn group_ceremony_authority(&self) -> Result<Option<GroupCeremonyAuthority>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::GroupCeremonyAuthority { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))
    }

    /// One row of the store's **meta table**, read on the store thread —
    /// `None` when the key was never written. A local read — never a
    /// network call.
    ///
    /// The meta table is the store's per-replica scratch (cursors, cached
    /// nest facts, the pending re-author marker …), **one owner per key**:
    /// this door is for a consumer that owns a key of its own and has no
    /// other way onto the store thread — the share pump's transfer ledger
    /// (`share_pump`) is the first — never for a key a pass reads or writes.
    /// Keys are namespaced by their owner (`<module>/<name>`), so two
    /// consumers cannot collide by accident.
    pub async fn meta_get(&self, key: impl Into<String>) -> Result<Option<Vec<u8>>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MetaGet {
                key: key.into(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Whether this replica's store records `scope` **listed** from its bound
    /// nest (`AccountStore::listed`) — the durable half of the first-listing
    /// gate, read on the store thread and answered inside a pass in flight
    /// (a local command). A reader does not call this: it crosses
    /// [`super::handle_source::first_listing_gate`], which also waits.
    pub async fn scope_listed(&self, scope: impl Into<String>) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::Listed {
                scope: scope.into(),
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Which sources of a held generation's key still stand for this device
    /// — the unkeyed hold's predicate (`crate::unkeyed_hold::unkeyed_hold`),
    /// read on the store thread over merged state, with this runtime's own
    /// identity and seed posture, and answered inside a pass in flight (a
    /// local command). A reader does not call this: it crosses
    /// [`super::handle_source::read_gate`], which also waits.
    pub async fn unkeyed_hold(&self) -> Result<crate::unkeyed_hold::HoldSources> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::UnkeyedHold { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// This process's first listings — the in-process half of the
    /// first-listing gate.
    pub(super) fn first_listings(&self) -> &crate::account_state_plane::FirstListings {
        &self.first_listings
    }

    /// Write one row of the store's meta table — the pair of
    /// [`Self::meta_get`], under the same one-owner-per-key contract: a key
    /// an account pass loads at its start and persists at its end is the
    /// pass's, and a caller writing it would race that pass exactly as a
    /// non-local command would. Durable when this answers; it answers inside
    /// a pass in flight (a local command — `Cmd::is_local`).
    pub async fn meta_put(&self, key: impl Into<String>, bytes: Vec<u8>) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::MetaPut {
                key: key.into(),
                bytes,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Land one BOUND `fauna.state.share-endpoints` row — the share sink's
    /// durable write (`ShareEndpointsSink`'s "a dial row is now durably
    /// cached" contract). The value must already have passed
    /// `fauna_peer_share::bind_share_advertisement`; the door derives the
    /// entry key from the bound ids, so key and value cannot diverge.
    pub async fn put_share_endpoints(
        &self,
        value: fauna_core::share_endpoints::ShareEndpoints,
    ) -> Result<u64> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::PutShareEndpoints { value, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// The peer leg's **cached last-known nest capabilities** — the offline
    /// brake evidence rule 7 names (`p2p.md` § Wormability walk, rule 7): the
    /// share plane's bind reads these when the live `fauna.nest.info` fetch
    /// fails, so a cold start with no nest reachable can still bind under the
    /// last-witnessed advertisement. `None` = no evidence at all (never
    /// fetched, or the cache is unreadable) — the bind's refusing arm, by
    /// design. One cache, one owner: the peer leg's pump writes it on every
    /// pass; this read only re-serves it. A local read — never a network call.
    pub async fn cached_nest_capabilities(&self) -> Result<Option<Vec<String>>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::CachedNestCapabilities { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))
    }

    /// This device's own p2p participation row
    /// ([`crate::p2p_participation`] — `p2p.md` § Per-device participation):
    /// the device's own choice, an absent row reading on. A local read —
    /// never a network call; the nest's brake reaches this row only through
    /// the pump's fold.
    pub async fn p2p_participation(&self) -> Result<crate::p2p_participation::P2pParticipation> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::P2pParticipation { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))
    }

    /// The user's own switch on this device — the write behind
    /// `device-p2p-participation-toggle` on this device's own row. Rests the
    /// row, then wakes the share driver ([`Self::p2p_participation_watch`]).
    /// The same-account leg reads the row at its next full pass; a caller
    /// that holds the engine may follow with [`Self::reconcile_now`] to make
    /// that pass now (`fauna_client_account_runtime::p2p_participation` does).
    pub async fn set_p2p_participation(&self, on: bool) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::SetP2pParticipation { on, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))??;
        self.p2p_participation_changed
            .send_modify(|n| *n = n.wrapping_add(1));
        Ok(())
    }

    /// A wake that fires after every [`Self::set_p2p_participation`] on this
    /// handle or any clone of it — what [`crate::share_glue::run`] selects on
    /// beside its tick. Carries a change counter, never the value.
    pub fn p2p_participation_watch(&self) -> watch::Receiver<u64> {
        self.p2p_participation_changed.subscribe()
    }

    /// `true` once this machine's store principal has its grant registered on
    /// the nest — the signal the principal's first connect waits on, so a
    /// first sign-in never mints `fauna.auth.device_handshake` into a
    /// `not_registered` refusal (`transport-connection.md` § The dial budget).
    ///
    /// Open from assembly whenever the slot already records a registration
    /// for the grant it carries (`PrincipalCustody::grant_registration_row` —
    /// every launch after a machine's first). Otherwise the engine holder's
    /// pass opens it once its enrollment verdict puts the grant on the nest,
    /// and a non-holder, which runs no pass, re-reads the shared slot's latch
    /// that the holder writes. Never closes again: a revoked grant is the
    /// supervisor's to meet, not a reason to stop dialling a connection that
    /// was once allowed.
    pub fn subscribe_grant_registered(&self) -> watch::Receiver<bool> {
        self.grant_registered.subscribe()
    }

    /// Report that a record's **body was handed to a visible view** — the T1
    /// browse trigger's app-side half ([`crate::observation_intake`], which
    /// owns the whole rule downstream of this call: classification, coordinate
    /// resolution, dedup, the class-2 put).
    ///
    /// This is the **only** thing an app contributes to the browse seen-set,
    /// and it is the one fact shared Rust cannot know. Two things follow, both
    /// load-bearing:
    ///
    /// - **Report display, never list membership.** A caller that can't tell
    ///   "realized in the buffer" from "on screen" is calling from the wrong
    ///   place — T1 names list-buffer transit, overscan and prefetch as
    ///   non-observations, and no rule downstream can recover the distinction.
    /// - **Reporting the same record twice is free.** The intake dedups against
    ///   the merged entry and returns [`ObservationOutcome::AlreadyIn`] without
    ///   publishing, so a shell may report its whole visible set every frame
    ///   rather than maintaining a "what did I already report" mirror.
    ///
    /// Local-first like every other write here: the entry is durable when this
    /// returns — even inside a pass in flight — and the publish step it arms
    /// ships it as soon as no pass is ([`Self::settled`] waits past it).
    pub async fn record_observation(&self, observation: Observation) -> Result<ObservationOutcome> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::RecordObservation { observation, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Raise the read marker (`fauna.state.read-marker`) of the fauna-native
    /// channel `channel_id_hex` to `through` — the **monotone raise door**
    /// (`conversation-read-state.md` § The read-marker record → *Who writes
    /// it*). A read-modify-write: the stored marker is joined with the raise
    /// (`ReadMarker::join`, a max-register), and a raise the stored value
    /// already covers writes nothing. `Ok(true)` = the marker moved and the
    /// row is durable locally (the publish step is armed, as a preference
    /// write arms it); `Ok(false)` = already covered.
    ///
    /// Not `put_preference`: that door is the singleton-key latest-wins
    /// path, and a stamped last-writer-wins value is exactly what could move a
    /// marker backwards. Served inside a pass at its yield points: the
    /// read-modify-write never straddles a walk page that merges the same
    /// entry, because nothing between its read and its write yields
    /// (`Cmd::is_local`, the read-marker verdict).
    pub async fn raise_read_marker(&self, channel_id_hex: &str, through: u64) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::RaiseReadMarker {
                channel_id_hex: channel_id_hex.to_string(),
                through,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Every fauna-native channel's read position this store holds —
    /// `(channel_id_hex, through)`, the keyed read of `fauna.state.read-marker`
    /// (`conversation-read-state.md` § How the carriers meet the in-memory
    /// set). A channel with no entry is absent (position `0`); an entry under
    /// another rail's keyspace, or one that does not decode, is skipped — a
    /// later build's rail is not this one's to read. A local read.
    pub async fn read_markers(&self) -> Result<Vec<(String, u64)>> {
        Ok(read_positions_of(
            &self
                .states_of_kind(fauna_protocol::merge_policy::KIND_READ_MARKER)
                .await?,
        ))
    }

    /// Wait for a pump pass to complete after the `after`-th, and answer the
    /// completed count then — the event a consumer re-reads a projection on
    /// when the plane has no per-kind change feed (`conversation-read-state.md`
    /// § The read-marker record → *How the manager reaches the plane*). Pair
    /// it with [`Self::pump_cycles`] for the baseline. Waits forever on a
    /// runtime that runs no pass ([`Self::is_engine_holder`] `false`) — that
    /// runtime's changes arrive through [`Self::data_version`] instead.
    pub async fn pass_completed_after(&self, after: u64) -> u64 {
        self.cycles.completed_after(after).await
    }

    /// This runtime's change generation: how many runs of its own pump have
    /// changed an entry a read can answer (`PumpCycles`' `generation` field
    /// owns what moves it). A plain atomic read, safe on a state-provider
    /// path. Pair it with [`Self::changed_after`] for the baseline; a store
    /// consumer reads it through the one shared watch
    /// (`fauna_account_seams::store_change`), never on its own.
    pub fn change_generation(&self) -> u64 {
        self.cycles.change_generation()
    }

    /// Wait for the change generation to move past `after`, and answer it
    /// then — the own-pump source of the store-change notice
    /// (`account-runtime.md` § Multi-instance concurrency → *A
    /// runtime's own pump is a source of the notice too*). Waits forever on
    /// a runtime whose pump changes nothing; another connection's commits
    /// arrive through [`Self::data_version`] instead.
    pub async fn changed_after(&self, after: u64) -> u64 {
        self.cycles.changed_after(after).await
    }

    /// Wake the pump for one scope. In production the pump's own push arm
    /// feeds this channel ([`AccountRuntimeParams::pushes`], mapped by
    /// [`nudge_scope_for_push`]); this door is for a caller that holds a wake
    /// the session's push stream does not carry (tests, a peer-leg signal).
    /// Fire-and-forget and coalescing: a full channel means a wake is already
    /// pending, which covers this one — the backstop ticker is the
    /// correctness path either way.
    pub fn nudge_scope(&self, scope: impl Into<String>) {
        match self.nudge.try_send(scope.into()) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {} // a wake is pending
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::debug!("account runtime nudge after shutdown — dropped");
            }
        }
    }

    /// Run one full pump pass now (reconcile + publish) and return
    /// what it did. The ticker's work on demand — also the latency-free
    /// barrier the tier_1 tests sequence on (e2e convention 14: a poke, not
    /// a sleep).
    pub async fn reconcile_now(&self) -> Result<PumpReport> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::ReconcileNow { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))
    }

    /// The registration chain moved (a kit ceremony created or replaced the
    /// RecoveryKey): carry it to every linked nest now, not at the next full
    /// pass (`identity-succession.md` § Enforcement on the home nest → *Every
    /// nest the identity is linked to*, clause (b)). Fire-and-forget: it
    /// queues a [`Self::reconcile_now`] whose reply nobody reads — a pass
    /// already runs the secondary leg's chain step per linked nest, so the
    /// wake needs no narrower command. A wake that cannot be queued (the
    /// command channel is full or the runtime is gone) is dropped: a nest the
    /// wake did not reach is owed until the next pass, which the backstop
    /// and every reconnect already run. Callable from sync code; it never
    /// awaits.
    pub fn registration_chain_moved(&self) {
        wake_chain_carry(&self.cmd);
    }

    /// The explicit pass barrier: resolves once no pass is in flight — the
    /// prologue included — and every command sent before it has been served.
    /// [`AccountStoreRuntime::start`] returns at assembly and the prologue
    /// runs after it, and a local command's round trip proves nothing about
    /// that pass (it is served inside it), so a caller that must sequence
    /// behind the prologue — a conformance test asserting what a later pass
    /// did — awaits this. Pass-bound by construction: it is parked with the
    /// other pass-bound commands and answered in arrival order between
    /// passes. (`reconcile_now` is a barrier too, and runs a pass of its
    /// own.) Resolves at once for a handle whose runtime is already gone.
    pub async fn settled(&self) {
        let (reply, rx) = oneshot::channel();
        if self.cmd.send(Cmd::Settled { reply }).await.is_ok() {
            let _ = rx.await;
        }
    }

    /// Deterministic teardown (account switch, quit-time drop, a superseded
    /// assembly): the store thread finishes its current step, drops the
    /// store, and exits. Idempotent — a second call reports success on the
    /// closed channel. **The machine stays enrolled**: its writer key and
    /// grant survive in the slot and on the nest, which is right whenever
    /// the slot survives too. A sign-out — the all-accounts erase — is
    /// [`Self::shutdown_for_sign_out`].
    pub async fn shutdown(&self) {
        let (reply, rx) = oneshot::channel();
        if self.cmd.send(Cmd::Shutdown { reply }).await.is_ok() {
            let _ = rx.await;
        }
    }

    /// Stage the durable intent to remove `removal.targets` with nest row
    /// `removal.row` — after [`Self::resolve_fleet_removal`], **before**
    /// `fauna.sync.devices.delete` (`crate::fleet_removal` § The completion
    /// rule owns the lifecycle). An error means nothing persisted: the caller
    /// must not delete, since a removal it cannot promise to finish is the
    /// leak this closes.
    pub async fn stage_fleet_removal(&self, removal: PendingFleetRemoval) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::StageFleetRemoval { removal, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Settle a staged removal on what the nest deletion came to: `Gone`
    /// journals every `Removed` row and then clears the intent, `Kept` clears
    /// it unwritten, `Unknown` leaves it for the pump's reconcile. An error
    /// (this call could not reach the runtime, a write failed) is worth
    /// showing but loses nothing: the intent is still staged, and the next
    /// full pass of whichever process pumps finishes it with no user gesture.
    pub async fn settle_fleet_removal(
        &self,
        removal: PendingFleetRemoval,
        outcome: NestDeletion,
    ) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::SettleFleetRemoval {
                removal,
                outcome,
                reply,
            })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Which fleet members removing nest row `row` must exclude — the devices
    /// page asks this **before** `fauna.sync.devices.delete`, so a refusal
    /// leaves the row in place to retry from. Resolved from client-held truth
    /// alone (`fauna_core::fleet_removal` owns the rule; `crate::fleet_removal`
    /// gathers the facts): the verified fleet view, each member's own sealed
    /// row statement, this device's id and enrolled row. `claimed` — the
    /// principal the nest put on the row — is only a claim to check. A runtime
    /// that is not up answers [`FleetRemovalRefusal::Unavailable`] rather than
    /// "nothing to remove": skipping the fleet leg is the leak.
    pub async fn resolve_fleet_removal(
        &self,
        row: impl Into<String>,
        claimed: Option<[u8; 32]>,
    ) -> Result<Vec<[u8; 32]>, FleetRemovalRefusal> {
        let gone = || FleetRemovalRefusal::Unavailable(RUNTIME_GONE.to_string());
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::ResolveFleetRemoval {
                row: row.into(),
                claimed,
                reply,
            })
            .await
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }

    /// The Devices page's read of the **member-addressed door** (clause (4),
    /// *A disagreement is the user's to settle*): this device's own fleet id
    /// — the fingerprint the page shows on this device's own row — and every
    /// verified member other than it that no roster row accounts for, each
    /// with the enrollment instant its own record asserts. `roster` is every
    /// nest row the page lists, as `(row id, claimed principal)`. Derived from
    /// client-held truth alone by the same rule the row gesture resolves with
    /// (`fauna_core::fleet_removal::unaccounted_members`; `crate::fleet_removal`
    /// gathers the facts): a member is listed exactly when no row gesture
    /// removes it and it alone. A local read.
    pub async fn unaccounted_fleet_members(
        &self,
        roster: Vec<(String, Option<[u8; 32]>)>,
    ) -> Result<FleetMembersView> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::UnaccountedFleetMembers { roster, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// The fleet ids this replica's merged device-set state excludes — the
    /// same derivation the peer leg's removed-device snapshot runs
    /// (`fleet_removal::removed_device_ids` over this runtime's store and
    /// `GenerationTrust`). The custody witness's mint reads it through the
    /// ceremony's registry-writer seam. A local read.
    pub async fn removed_device_ids(&self) -> Result<std::collections::HashSet<[u8; 32]>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::RemovedDeviceIds { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// The member-addressed door's one leg: the user picked `member` by its
    /// fleet id (`device-member-remove-button`), and that id is the target —
    /// no nest input, so a hostile nest cannot aim it; no row statement read,
    /// so the member cannot veto it. Resolved by
    /// `fauna_core::fleet_removal::resolve_member_removal` (this device's own
    /// id → `OwnDevice`; an id the fleet view does not verify → `NotAMember`;
    /// an id already removed writes nothing and answers `Ok`) and written
    /// through `fleet_removal::write_removed`, the honest writer `settle` and
    /// `complete_pending` share.
    /// Nothing to stage: no nest row is chosen, so there is no nest deletion
    /// to bracket — a failed write answers [`FleetRemovalRefusal::Unavailable`]
    /// and the member stays listed to retry from. A runtime that is not up
    /// answers `Unavailable` too.
    pub async fn remove_fleet_member(&self, member: [u8; 32]) -> Result<(), FleetRemovalRefusal> {
        let gone = || FleetRemovalRefusal::Unavailable(RUNTIME_GONE.to_string());
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::RemoveFleetMember { member, reply })
            .await
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }

    /// Retire this machine's enrollment nest-side, then [`Self::shutdown`] —
    /// the teardown a **sign-out** runs, and only a sign-out
    /// (`sync-agent-credentials.md` § Credential model → *The signed-out
    /// reconcile*, the nest-side leg).
    ///
    /// **Why the ordering, and why it is one call.** The retirement needs
    /// two things a sign-out is about to destroy: the writer key, which the
    /// erase that follows sweeps from the slot, and an authenticated session
    /// of the account, which the app drops with the window. Both live in the
    /// running runtime, so the retirement rides its last command before the
    /// store closes. It is a proof-of-possession revoke by the key itself
    /// (`fauna_client_sync::build_grant_revoke_request`), which authorizes
    /// nothing new — the key already mints bearers — and can only destroy
    /// this machine's own credential.
    ///
    /// **Never on an account switch.** A switch keeps the slot, so the key
    /// it holds will be loaded again at the next sign-in as this account;
    /// tombstoning it would make that sign-in a *removed-from-account*
    /// state ([`EnrollmentPass::RemovedFromAccount`]) and cost a successor
    /// mint for an ordinary switch. The reason is the caller's to state
    /// (`fauna_client_account_runtime::StopReason`), never inferred here.
    ///
    /// Bounded ([`ENROLLMENT_RETIRE_BUDGET`]) and best-effort: a nest that
    /// cannot be reached, or lacks the kind, answers
    /// [`EnrollmentRetirement::Deferred`] and the shutdown runs regardless —
    /// the user asked to be signed out.
    ///
    /// **Never behind a pass.** The retirement is a pass-bound command
    /// (`Cmd::is_local`): it ends the principal's sessions, so it is parked
    /// until the pass in flight ends — which, for a sign-out landing just
    /// after a sign-in, is the prologue, the longest pass there is. So the
    /// sign-out raises its claim first, and the pass is cut
    /// [`SIGN_OUT_PASS_GRACE`] later at its next yield point — every unit of
    /// local work in a pass ends in one (`pass_breath`) — and the retirement
    /// then runs within the stop.
    pub async fn shutdown_for_sign_out(&self) -> EnrollmentRetirement {
        // The first request wins: a second stop of the same runtime (the
        // host's superseded arm racing its teardown) must not move the cut.
        self.sign_out.send_if_modified(|requested| {
            let first = requested.is_none();
            if first {
                *requested = Some(now_ms());
            }
            first
        });
        let (reply, rx) = oneshot::channel();
        let retirement = if self.cmd.send(Cmd::RetireEnrollment { reply }).await.is_ok() {
            rx.await
                .unwrap_or_else(|_| EnrollmentRetirement::Deferred(RUNTIME_GONE.into()))
        } else {
            EnrollmentRetirement::Deferred(RUNTIME_GONE.into())
        };
        self.shutdown().await;
        retirement
    }

    /// The dead generations of this account's fleet scope — each keyed by no
    /// device, listed in no verified member's reach and wrapped by no holder
    /// — with its mint stamp and live-row count: what the Settings
    /// recovery-kit section renders its let-go from, only while this answers
    /// non-empty (`crate::generation_let_go`; `account-data-taxonomy.md`
    /// § The generation machinery → *Fleet-scope reclamation*, clause
    /// (3)(j)). Pass-bound: it may ask the holder.
    pub async fn dead_generations(&self) -> Result<Vec<crate::generation_let_go::DeadGeneration>> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::DeadGenerations { reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Let go of `generations` — the user's confirmed act, and the only
    /// caller there is: the dead read is re-checked first, and a generation
    /// that no longer reads dead is reported refused and left untouched
    /// (`crate::generation_let_go::let_go`). Idempotent; a repeat finishes
    /// what a deferred retire left. Pass-bound.
    pub async fn let_go(
        &self,
        generations: std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<crate::generation_let_go::LetGoReport> {
        let (reply, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::LetGo { generations, reply })
            .await
            .map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?;
        rx.await.map_err(|_| anyhow::anyhow!(RUNTIME_GONE))?
    }

    /// Resolves once the runtime has shut down — the store thread exited and
    /// dropped its command receiver — so a task serving this account can end
    /// on the *event* rather than discover it at its next tick
    /// (`share_glue::run` selects on it beside its cadence). Resolves at once
    /// for a handle whose runtime is already gone.
    pub async fn closed(&self) {
        self.cmd.closed().await
    }
}

/// The ceremony driver's registry-row seam (
/// `fauna_client_capabilities::custody_ceremony::drive_ceremonies`),
/// implemented directly on the runtime handle so every app wires the door
/// with zero per-app glue (priority #1): a put is the typed store-thread
/// door above, and a refusal (no generation tip resolves yet; runtime gone)
/// answers `false` — the driver keeps the row owed and retries.
impl fauna_client_capabilities::custody_ceremony::CustodyRegistryWriter for AccountStoreHandle {
    async fn put_custodian_endpoints(
        &self,
        value: &fauna_core::custodian_endpoints::CustodianEndpoints,
    ) -> bool {
        match AccountStoreHandle::put_custodian_endpoints(self, value.clone()).await {
            Ok(_) => true,
            Err(e) => {
                tracing::debug!("custodian-endpoints row put owed: {e:#}");
                false
            }
        }
    }

    async fn put_custodies_held(&self, value: &fauna_core::custodies_held::CustodyHeld) -> bool {
        match AccountStoreHandle::put_custodies_held(self, value.clone()).await {
            Ok(_) => true,
            Err(e) => {
                tracing::debug!("custodies-held row put owed: {e:#}");
                false
            }
        }
    }

    /// The witness's exclusion list: this device's own verified fleet view.
    /// A read that fails (no trust derived yet, runtime gone) answers empty —
    /// the mint never waits on it.
    async fn removed_device_ids(&self) -> Vec<[u8; 32]> {
        match AccountStoreHandle::removed_device_ids(self).await {
            Ok(set) => set.into_iter().collect(),
            Err(e) => {
                tracing::debug!(
                    "custody witness: fleet view underived, empty exclusion list: {e:#}"
                );
                Vec::new()
            }
        }
    }
}

impl AccountStoreHandle {
    fn ledger_identity(&self) -> Result<&LedgerIdentity, fauna_client_config::StoreError> {
        self.ledger_identity.as_deref().ok_or_else(|| {
            fauna_client_config::StoreError::Load(
                "succession ledger: this runtime's account id did not decode".into(),
            )
        })
    }
}

/// The succession-ledger seam (`fauna_client_config::SuccessionLedgerStore`),
/// implemented directly on the runtime handle so every app wires it with zero
/// per-app glue (priority #1), exactly as [`CustodyRegistryWriter`] above: a
/// read is the handle's READ fold as this runtime's identity sees it, a write
/// is the door's per-row join with this runtime's attested signer set, and a
/// door refusal (no generation tip resolves yet, an unattested event, a forked
/// chain; the runtime gone) surfaces as `StoreError::Save` — the caller's leg
/// stays owed.
///
/// [`CustodyRegistryWriter`]: fauna_client_capabilities::custody_ceremony::CustodyRegistryWriter
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_client_config::SuccessionLedgerStore for AccountStoreHandle {
    fn self_actor(&self) -> Result<fauna_core::identity::ActorId, fauna_client_config::StoreError> {
        Ok(self.ledger_identity()?.self_actor)
    }

    async fn load(
        &self,
    ) -> Result<fauna_core::succession_ledger::SuccessionLedger, fauna_client_config::StoreError>
    {
        let identity = self.ledger_identity()?;
        self.succession_ledger(identity.self_actor)
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn merge(
        &self,
        replica: fauna_core::succession_ledger::SuccessionLedger,
    ) -> Result<fauna_core::succession_ledger::SuccessionLedger, fauna_client_config::StoreError>
    {
        let identity = self.ledger_identity()?;
        self.merge_succession_ledger(identity.self_actor, replica, identity.attested.clone())
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }

    async fn repoint(
        &self,
        retired: fauna_core::identity::ActorId,
    ) -> Result<bool, fauna_client_config::StoreError> {
        let identity = self.ledger_identity()?;
        self.repoint_succession_ledger(retired, identity.self_actor)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }

    async fn raise_grant_marks(
        &self,
        predecessor: fauna_core::identity::ActorId,
    ) -> Result<bool, fauna_client_config::StoreError> {
        let identity = self.ledger_identity()?;
        AccountStoreHandle::raise_grant_marks(self, identity.self_actor, predecessor)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }
}

/// The kind-manifest seam (`fauna_client_config::KindManifestStore`),
/// implemented directly on the runtime handle like the ledger seam above, so
/// the consent-time mint runs over it with zero per-app glue.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_client_config::KindManifestStore for AccountStoreHandle {
    async fn admitted_kinds(
        &self,
    ) -> Result<fauna_protocol::merge_policy::AdmittedKinds, fauna_client_config::StoreError> {
        AccountStoreHandle::admitted_kinds(self)
            .await
            .map(|overlay| overlay.kinds)
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn publish(
        &self,
        client_id: &str,
        manifest: &fauna_protocol::kind_manifest::VerifiedManifest,
        admitted_at_ms: i64,
    ) -> Result<(), fauna_client_config::StoreError> {
        self.write_kind_manifest(client_id, manifest, admitted_at_ms)
            .await
            .map(|_| ())
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }
}

/// The deployment-seed custody seam
/// (`fauna_client_config::DeploymentSeedStore`), implemented directly on the
/// runtime handle for the same reason as the ledger seam above: the custody
/// leg and the plane rotation drive run over it with zero per-app glue. A
/// read is the handle's fold, a write the door's per-row join, and a door
/// refusal (no generation tip resolves yet; an entry the plane would refuse;
/// the runtime gone) surfaces as `StoreError::Save` — the caller's write
/// stays owed.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_client_config::DeploymentSeedStore for AccountStoreHandle {
    async fn seeds(
        &self,
    ) -> Result<Vec<fauna_core::data::DeploymentSeedEntry>, fauna_client_config::StoreError> {
        self.deployment_seeds()
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn merge_seeds(
        &self,
        replica: Vec<fauna_core::data::DeploymentSeedEntry>,
    ) -> Result<Vec<fauna_core::data::DeploymentSeedEntry>, fauna_client_config::StoreError> {
        self.merge_deployment_seeds(replica)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }

    async fn seed_published(
        &self,
        nest_actor_id: [u8; 32],
    ) -> Result<bool, fauna_client_config::StoreError> {
        self.deployment_seed_published(nest_actor_id)
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }
}

/// The period-key store seam (`fauna_client_subscriptions::PeriodKeyStore`),
/// implemented directly on the runtime handle so every app wires the custody
/// with zero per-app glue (priority #1), exactly as the ledger seam above: a
/// read is the handle's READ fold, a write the door's per-row join, a settle
/// the door's settle; a refusal (no generation tip resolves yet, a row that
/// will not decode, the runtime gone) surfaces as a `StoreError` — never as
/// an empty custody.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_client_subscriptions::PeriodKeyStore for AccountStoreHandle {
    async fn custody(
        &self,
    ) -> Result<fauna_core::data::SubscriptionsConfig, fauna_client_config::StoreError> {
        self.subscriptions()
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn merge_custody(
        &self,
        replica: fauna_core::data::SubscriptionsConfig,
    ) -> Result<fauna_core::data::SubscriptionsConfig, fauna_client_config::StoreError> {
        self.merge_subscriptions(replica)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }

    async fn settle_removal(
        &self,
        removal: fauna_core::data::PendingRemoval,
    ) -> Result<fauna_core::data::SubscriptionsConfig, fauna_client_config::StoreError> {
        self.settle_pending_removal(removal)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }
}

/// The custody-ceremony seam
/// (`fauna_client_config::CustodyCeremonyStore`), implemented directly on the
/// runtime handle for the same reason as the ledger seam above: the ceremony
/// drive, its conversations sink and the custody acts run over it with zero
/// per-app glue, and web's core chunk serves it across the account port. A
/// read is the handle's fold, a write the door's per-record join; a door
/// refusal (no generation tip resolves yet; the runtime gone) surfaces as
/// `StoreError::Save` — the caller's write stays owed.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_client_config::CustodyCeremonyStore for AccountStoreHandle {
    async fn custody(
        &self,
    ) -> Result<fauna_core::custody_ceremony::CustodyConfig, fauna_client_config::StoreError> {
        AccountStoreHandle::custody(self)
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn merge_custody(
        &self,
        replica: fauna_core::custody_ceremony::CustodyConfig,
    ) -> Result<fauna_core::custody_ceremony::CustodyConfig, fauna_client_config::StoreError> {
        AccountStoreHandle::merge_custody(self, replica)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }
}

/// The followed-folders seam (`fauna_client_config::FollowsStore`),
/// implemented directly on the runtime handle for the same reason as the
/// custody-ceremony seam above: the follow recipes and the followed-folders
/// source run over it with zero per-app glue, and web's core chunk serves it
/// across the account port. A read is the handle's fold, a write one row
/// through the door; a door refusal (no generation tip resolves yet; the
/// runtime gone) surfaces as `StoreError::Save`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_client_config::FollowsStore for AccountStoreHandle {
    async fn follows(
        &self,
    ) -> Result<fauna_core::data::FollowsConfig, fauna_client_config::StoreError> {
        AccountStoreHandle::follows(self)
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn put_follow(
        &self,
        follow: fauna_core::data::FollowedFolder,
    ) -> Result<bool, fauna_client_config::StoreError> {
        AccountStoreHandle::put_follow(self, follow)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }

    async fn unfollow(
        &self,
        home_nest_url: String,
        folder_id: i64,
    ) -> Result<bool, fauna_client_config::StoreError> {
        AccountStoreHandle::unfollow(self, home_nest_url, folder_id)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }
}

/// **The mail custody seam, served by the runtime's own doors** — the
/// `fauna.state.mail` READ fold and the four per-row writes
/// (`crate::mail_rows` owns the mechanics). A door refusal, the transient
/// no-tip refusal included, surfaces as `StoreError::Save`.
///
/// **A read crosses the read gate** ([`super::handle_source::read_gate`]),
/// here on the seam so every consumer is covered, whether it holds this
/// handle or the sourced [`super::handle_source::AccountMailStore`] over it:
/// a replica that has never listed the fleet scope, or one that holds rows
/// under a generation it may still be keyed for (the unkeyed hold), would
/// answer an empty custody — a mailbox other devices enabled reading as
/// disabled, which the page then offers to enable and the succession burn
/// reads as "no live credential" — so it is refused as not ready
/// (`StoreError::is_not_ready`) instead. A write needs no gate:
/// it is a read-join-put at the door, so it never loses rows the listing
/// lands later.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_client_config::MailStore for AccountStoreHandle {
    async fn load(&self) -> Result<fauna_core::data::MailConfig, fauna_client_config::StoreError> {
        super::handle_source::read_gate(self, fauna_protocol::merge_policy::KIND_MAIL)
            .await
            .map_err(super::handle_source::ScopeNotReady::into_load)?;
        self.mail()
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn load_rows(
        &self,
    ) -> Result<fauna_core::mail_rows::MailRows, fauna_client_config::StoreError> {
        super::handle_source::read_gate(self, fauna_protocol::merge_policy::KIND_MAIL)
            .await
            .map_err(super::handle_source::ScopeNotReady::into_load)?;
        self.mail_rows()
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn write_state(
        &self,
        state: fauna_core::mail_rows::MailStateRow,
    ) -> Result<bool, fauna_client_config::StoreError> {
        self.write_mail_state(state)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }

    async fn put_credential(
        &self,
        credential: fauna_core::data::MailCredential,
    ) -> Result<bool, fauna_client_config::StoreError> {
        self.put_mail_credential(credential)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }

    async fn mark_wrapped(
        &self,
        credential_id: String,
        fingerprint: fauna_core::data::MsekFingerprint,
    ) -> Result<bool, fauna_client_config::StoreError> {
        self.mark_mail_credential_wrapped(credential_id, fingerprint)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }

    async fn revoke(&self, credential_id: String) -> Result<bool, fauna_client_config::StoreError> {
        self.revoke_mail_credential(credential_id)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }
}

/// The backup-destination state seam (`fauna_client_config::BackupStateStore`)
/// on the runtime handle, for the same reason as the ledger seam above: the
/// Backups page's shared sequences, the trust facet and the succession
/// aftermath's backup legs run over it with zero per-app glue. A read is the
/// handle's fold for one box; a write the door's (the list stamped above the
/// stored row, a mark joined into its row); a door refusal (the row's bounds,
/// no generation tip resolves yet, the runtime gone) surfaces as
/// `StoreError::Save`.
///
/// **A read crosses the read gate** ([`super::handle_source::read_gate`]), as
/// the mail custody's does: the kind is fleet-only and tip-sealed, so a
/// freshly signed-in device — and a successor, whose list its predecessor
/// wrote — holds no row until its first listing lands, and a listed device
/// that has not keyed the account's generation cannot open the row it holds.
/// A read ahead of either would answer an empty list, which the enrollment
/// heal reads as "nothing configured" and the aftermath's `NestBackupKey`
/// re-grant as "nothing owed"; it is refused as not ready
/// (`StoreError::is_not_ready`) instead. A write needs no gate of its own: every list write is
/// `mutate_backup`'s read-edit-write, whose read crossed it.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_client_config::BackupStateStore for AccountStoreHandle {
    async fn backup_state(
        &self,
        source_nest: [u8; 32],
    ) -> Result<fauna_core::backup_state::BackupState, fauna_client_config::StoreError> {
        super::handle_source::read_gate(self, fauna_protocol::merge_policy::KIND_BACKUP)
            .await
            .map_err(super::handle_source::ScopeNotReady::into_load)?;
        AccountStoreHandle::backup_state(self, source_nest)
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn backup_destination_lists(
        &self,
    ) -> Result<Vec<fauna_core::backup_state::BackupDestinationsRow>, fauna_client_config::StoreError>
    {
        super::handle_source::read_gate(self, fauna_protocol::merge_policy::KIND_BACKUP)
            .await
            .map_err(super::handle_source::ScopeNotReady::into_load)?;
        AccountStoreHandle::backup_destination_lists(self)
            .await
            .map_err(|e| fauna_client_config::StoreError::Load(format!("{e:#}")))
    }

    async fn write_backup_destinations(
        &self,
        source_nest: [u8; 32],
        backup: fauna_core::data::BackupConfig,
    ) -> Result<fauna_core::backup_state::BackupState, fauna_client_config::StoreError> {
        AccountStoreHandle::write_backup_destinations(self, source_nest, backup)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }

    async fn merge_destination_marks(
        &self,
        marks: Vec<fauna_core::data::DestinationUnattestedMark>,
    ) -> Result<Vec<fauna_core::data::DestinationUnattestedMark>, fauna_client_config::StoreError>
    {
        AccountStoreHandle::merge_destination_marks(self, marks)
            .await
            .map_err(|e| fauna_client_config::StoreError::Save(format!("{e:#}")))
    }
}

/// What a local command may touch (`Cmd::is_local`): the store, both
/// planes, the assembly's writer identity and principal slot, the app-fed
/// scope registrations and endpoint facts (every pass reads a snapshot of
/// both, taken at its start) — nothing a pass borrows exclusively, nothing a
/// pass loads at its start and persists at its end.
pub(crate) struct LocalCtx<'a, B: StoreBackend, R: RpcRequester> {
    pub(crate) store: &'a AccountStore<B>,
    pub(crate) plane: &'a AccountStatePlane<'a, B, R>,
    pub(crate) fleet_plane: &'a AccountStatePlane<'a, B, R>,
    pub(crate) trust: &'a crate::generation_tip::GenerationTrust,
    pub(crate) writer_key: &'a SigningKey,
    pub(crate) principal_slot: &'a dyn PrincipalCustody,
    /// Whether this runtime holds the identity seed — what the unkeyed
    /// hold's holder source needs (`crate::unkeyed_hold::HoldContext`).
    pub(crate) seed_holding: bool,
    /// The assembly's bind-leg memory, for the pinned holders' verified
    /// ancestors the holder source counts.
    pub(crate) bind: &'a crate::bind_leg::BindMemo,
    pub(crate) registered: &'a mut Vec<ContentScope>,
    pub(crate) endpoint_facts: &'a mut Option<EndpointFacts>,
    /// Set by a local write: the publish step is owed as soon as no pass is
    /// in flight (the serve loop's top).
    pub(crate) publish_due: &'a mut bool,
    /// Whether a pass (or the publish step) is in flight around this
    /// command — the one case a tip-sealed door put that would mint is
    /// parked rather than served (`Cmd::is_local`, the door-put verdict).
    pub(crate) pass_in_flight: bool,
}

/// How [`serve_local_cmd`] ended: served; the writer was rotated under
/// this process and the caller reassembles (the command was answered with
/// the typed refusal; its caller retries); or — inside a pass only — the
/// command is handed back unserved, to be parked behind the pass.
pub(crate) enum Served {
    Done,
    Reassemble,
    Park(Cmd),
}

/// Serve one local command (`Cmd::is_local`) — inside a pass at one of its
/// yield points, or between passes; the same code either way.
pub(crate) async fn serve_local_cmd<B, R>(cmd: Cmd, local: &mut LocalCtx<'_, B, R>) -> Served
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    /// The typed stale-writer refusal on a local write's answer: reply, then
    /// tell the loop to reassemble (the caller retries).
    fn stale_writer_in<T>(r: &Result<T>) -> bool {
        r.as_ref()
            .err()
            .is_some_and(fauna_account_store::store::is_stale_writer)
    }
    /// A local write's answer: arm the publish step on `publish`, reply,
    /// and reassemble on the typed stale-writer refusal.
    fn wrote<T>(
        r: Result<T>,
        publish: impl FnOnce(&T) -> bool,
        publish_due: &mut bool,
        reply: oneshot::Sender<Result<T>>,
    ) -> Served {
        let stale = stale_writer_in(&r);
        if r.as_ref().is_ok_and(publish) {
            *publish_due = true;
        }
        let _ = reply.send(r);
        if stale {
            tracing::info!(
                "account runtime: writer rotated under this process — \
                 reassembling to adopt the successor (caller retries)"
            );
            return Served::Reassemble;
        }
        Served::Done
    }
    // The door-put verdict: inside a pass, a tip-sealed put is served only
    // when its door will not mint. The answer holds for the write below —
    // both are store work, and no step of the pass runs between them (it is
    // not polled while a command is served). An error here parks too: the
    // put itself answers it, after the pass.
    if local.pass_in_flight
        && let Some(kind) = cmd.tip_sealed_kind()
        && !matches!(local.fleet_plane.origination_mints(kind).await, Ok(false))
    {
        return Served::Park(cmd);
    }
    let me = local.writer_key.verifying_key().to_bytes();
    match cmd {
        Cmd::SetEndpointFacts { facts, reply } => {
            *local.endpoint_facts = Some(facts);
            let _ = reply.send(());
        }
        // A caller-owned meta-table row (the share pump's transfer ledger is
        // the first owner) — one meta-table write on this store.
        Cmd::MetaPut { key, bytes, reply } => {
            let _ = reply.send(local.store.backend().meta_put(&key, &bytes).await);
        }
        Cmd::MetaGet { key, reply } => {
            let _ = reply.send(local.store.backend().meta_get(&key).await);
        }
        // Read off the store, never off this process's own listings: a
        // runtime that pumps no pass answers what its engine holder recorded.
        Cmd::Listed { scope, reply } => {
            let _ = reply.send(local.store.listed(&scope).await);
        }
        Cmd::UnkeyedHold { reply } => {
            let ancestors = local.bind.ancestors().unwrap_or_default();
            let cx = crate::unkeyed_hold::HoldContext {
                trust: local.trust,
                writer_key: local.writer_key,
                custody: local.fleet_plane.generation_custody(),
                seed_holding: local.seed_holding,
                ancestors: &ancestors,
            };
            let _ = reply.send(crate::unkeyed_hold::unkeyed_hold(local.store, &cx).await);
        }
        // Serialized per scope by this one store thread, which runs each
        // intake whole before the next command (`record_observation`'s
        // read-modify-write is not a CAS).
        Cmd::RecordObservation { observation, reply } => {
            let r = record_observation(local.store, local.plane, &observation).await;
            return wrote(
                r,
                |o| *o == ObservationOutcome::Recorded,
                local.publish_due,
                reply,
            );
        }
        Cmd::RaiseReadMarker {
            channel_id_hex,
            through,
            reply,
        } => {
            let r =
                raise_read_marker_local(local.store, local.plane, &channel_id_hex, through).await;
            return wrote(r, |moved| *moved, local.publish_due, reply);
        }
        // The custody registry rows and the share leg's cached
        // discovery row (the sink's durable write — `p2p.md` § Built — the
        // discovery carriage; the row arrives already BOUND to the
        // MLS-authenticated sender): typed door-puts on the fleet plane,
        // `custody_rows` owns the mechanics.
        Cmd::PutCustodianEndpoints { value, reply } => {
            let r =
                crate::custody_rows::put_custodian_endpoints(local.fleet_plane, me, &value).await;
            return wrote(r, |_| true, local.publish_due, reply);
        }
        Cmd::PutShareEndpoints { value, reply } => {
            let r = crate::custody_rows::put_share_endpoints(local.fleet_plane, me, &value).await;
            return wrote(r, |_| true, local.publish_due, reply);
        }
        // The private contact overlay: a read-modify-write on this one store
        // thread (nothing between the read and the put yields), through the
        // fleet plane's writer door — `contact_overlay_rows` owns the mechanics.
        Cmd::WriteContactOverlay {
            actor_id_hex,
            write,
            reply,
        } => {
            let r = crate::contact_overlay_rows::write_contact_overlay(
                local.store,
                local.fleet_plane,
                me,
                &actor_id_hex,
                &write,
                fauna_core::data::Timestamp::now_millis_or_zero() as i64,
            )
            .await;
            return wrote(
                r,
                |o| {
                    matches!(
                        o,
                        crate::contact_overlay_rows::OverlayWriteOutcome::Written(_)
                    )
                },
                local.publish_due,
                reply,
            );
        }
        Cmd::FoldContactOverlay {
            predecessor_hex,
            successor_hex,
            reply,
        } => {
            let r = crate::contact_overlay_rows::fold_contact_overlay(
                local.store,
                local.fleet_plane,
                &predecessor_hex,
                &successor_hex,
            )
            .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        // The DNS record: a whole-record LWW put through the fleet plane's
        // writer door — `dns_rows` owns the mechanics.
        Cmd::WriteDns { next, reply } => {
            let r = crate::dns_rows::write_dns(local.store, local.fleet_plane, me, &next).await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        // The kind-manifest row: a whole-row LWW put through the fleet plane's
        // writer door — `kind_manifest_rows` owns the mechanics.
        Cmd::WriteKindManifest {
            client_id,
            manifest,
            admitted_at_ms,
            reply,
        } => {
            let r = crate::kind_manifest_rows::write_kind_manifest(
                local.fleet_plane,
                &client_id,
                &manifest,
                admitted_at_ms,
                me,
            )
            .await;
            return wrote(r, |_| true, local.publish_due, reply);
        }
        // The app credentials: a per-credential LWW put or stamped tombstone
        // through the fleet plane's writer door — `atproto_rows` owns the
        // mechanics.
        Cmd::PutAppCredential { credential, reply } => {
            let r = crate::atproto_rows::put_app_credential(
                local.store,
                local.fleet_plane,
                me,
                &credential,
            )
            .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        Cmd::RevokeAppCredential {
            credential_id,
            reply,
        } => {
            let r = crate::atproto_rows::revoke_app_credential(
                local.store,
                local.fleet_plane,
                me,
                &credential_id,
            )
            .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        // The followed folders: a per-folder LWW put or stamped tombstone
        // through the fleet plane's writer door — `follows_rows` owns the
        // mechanics.
        Cmd::PutFollow { follow, reply } => {
            let r =
                crate::follows_rows::put_follow(local.store, local.fleet_plane, me, &follow).await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        Cmd::Unfollow {
            home_nest_url,
            folder_id,
            reply,
        } => {
            let r = crate::follows_rows::unfollow(
                local.store,
                local.fleet_plane,
                me,
                &home_nest_url,
                folder_id,
            )
            .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        // The mail custody: per-row read-join-puts on this one store thread —
        // `mail_rows` owns the mechanics.
        Cmd::WriteMailState { state, now, reply } => {
            let r = crate::mail_rows::write_mail_state(local.store, local.fleet_plane, &state, now)
                .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        Cmd::PutMailCredential {
            credential,
            now,
            reply,
        } => {
            let r =
                crate::mail_rows::put_credential(local.store, local.fleet_plane, &credential, now)
                    .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        Cmd::MarkMailCredentialWrapped {
            credential_id,
            fingerprint,
            now,
            reply,
        } => {
            let r = crate::mail_rows::mark_credential_wrapped(
                local.store,
                local.fleet_plane,
                &credential_id,
                fingerprint,
                now,
            )
            .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        Cmd::RevokeMailCredential {
            credential_id,
            now,
            reply,
        } => {
            let r = crate::mail_rows::revoke_credential(
                local.store,
                local.fleet_plane,
                &credential_id,
                now,
            )
            .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        // The offline-share ceremony record: a read-join-put on this one
        // store thread — `group_share_rows` owns the mechanics.
        Cmd::MergeGroupShares { replica, reply } => {
            let r = crate::group_share_rows::merge_group_shares(
                local.store,
                local.fleet_plane,
                &replica,
            )
            .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(joined, _)| joined),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        // The backup-destination state: the list's read-stamp-put and the
        // marks' read-join-put, each whole on this one store thread —
        // `backup_rows` owns the mechanics.
        Cmd::WriteBackupDestinations {
            source_nest,
            backup,
            reply,
        } => {
            let r = crate::backup_rows::write_backup_destinations(
                local.store,
                local.fleet_plane,
                &source_nest,
                &backup,
                fauna_core::data::Timestamp::now(),
            )
            .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(state, _)| state),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        Cmd::MergeDestinationMarks { marks, reply } => {
            let r =
                crate::backup_rows::merge_destination_marks(local.store, local.fleet_plane, &marks)
                    .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(state, _)| state),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        // The succession ledger: read-join-puts on this one store thread —
        // `succession_ledger_rows` owns the mechanics.
        Cmd::MergeSuccessionLedger {
            self_actor,
            replica,
            attested,
            reply,
        } => {
            let r = crate::succession_ledger_rows::merge_succession_ledger(
                local.store,
                local.fleet_plane,
                self_actor,
                &replica,
                &attested,
            )
            .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(joined, _)| joined),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        Cmd::RepointSuccessionLedger {
            retired,
            successor,
            reply,
        } => {
            let r = crate::succession_ledger_rows::repoint_succession_ledger(
                local.store,
                local.fleet_plane,
                retired,
                successor,
            )
            .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        Cmd::RaiseGrantMarks {
            self_actor,
            predecessor,
            reply,
        } => {
            let r = crate::succession_ledger_rows::raise_grant_marks(
                local.store,
                local.fleet_plane,
                self_actor,
                predecessor,
            )
            .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        // The period-key custody: a per-row read-join-put on this one store
        // thread — `subscription_rows` owns the mechanics.
        Cmd::MergeSubscriptions { replica, reply } => {
            let r = crate::subscription_rows::merge_subscriptions(
                local.store,
                local.fleet_plane,
                &replica,
            )
            .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(joined, _)| joined),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        Cmd::SettlePendingRemoval { removal, reply } => {
            let r = crate::subscription_rows::settle_pending_removal(
                local.store,
                local.fleet_plane,
                &removal,
            )
            .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(joined, _)| joined),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        // Folder-key custody: a read-join-put on this one store thread —
        // `folder_key_rows` owns the mechanics.
        Cmd::MergeFolderKeys { replica, reply } => {
            let r =
                crate::folder_key_rows::merge_folder_keys(local.store, local.fleet_plane, &replica)
                    .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(joined, _)| joined),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        Cmd::SettleFolderRemoval { removal, reply } => {
            let r = crate::folder_key_rows::settle_pending_removal(
                local.store,
                local.fleet_plane,
                &removal,
            )
            .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(joined, _)| joined),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        // The refused-change list: a read-modify-put on this one store
        // thread — `refused_change_rows` owns the mechanics.
        Cmd::WriteRefusedChanges { write, reply } => {
            let r = crate::refused_change_rows::write_refused_scheduling_changes(
                local.store,
                local.fleet_plane,
                write,
            )
            .await;
            return wrote(r, |moved| *moved, local.publish_due, reply);
        }
        // The deployment-seed custody: a per-row read-join-put on this one
        // store thread — `deployment_seed_rows` owns the mechanics.
        Cmd::MergeDeploymentSeeds { replica, reply } => {
            let r = crate::deployment_seed_rows::merge_deployment_seeds(
                local.store,
                local.fleet_plane,
                &replica,
            )
            .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(joined, _)| joined),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        // The peer-anchor cache: a read-join-put on this one store thread —
        // `peer_anchor_rows` owns the mechanics.
        Cmd::MergePeerAnchors { replica, reply } => {
            let r = crate::peer_anchor_rows::merge_peer_anchors(
                local.store,
                local.fleet_plane,
                &replica,
            )
            .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(joined, _)| joined),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        // The blessed nests: a read-stamp-put on this one store thread —
        // `blessed_nest_rows` owns the mechanics.
        Cmd::SetNestBlessed {
            nest_id,
            blessed,
            now,
            reply,
        } => {
            let r = crate::blessed_nest_rows::set_nest_blessed(
                local.store,
                local.fleet_plane,
                &nest_id,
                blessed,
                now,
            )
            .await;
            return wrote(r, |moved| *moved, local.publish_due, reply);
        }
        // The ATProto identity custody: a read-join-put on this one store
        // thread — `atproto_identity_rows` owns the mechanics.
        Cmd::MergeAtprotoIdentity { replica, reply } => {
            let r = crate::atproto_identity_rows::merge_atproto_identity(
                local.store,
                local.fleet_plane,
                &replica,
            )
            .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(joined, _)| joined),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        // The npub confirmation: a read-join-put on this one store thread —
        // `nostr_confirmation_rows` owns the mechanics.
        Cmd::ConfirmNostrNpub { now, reply } => {
            let r = crate::nostr_confirmation_rows::confirm_nostr_npub(
                local.store,
                local.fleet_plane,
                now,
            )
            .await;
            return wrote(r, |written| *written, local.publish_due, reply);
        }
        // The custody ceremony state: a read-join-put on this one store
        // thread — `custody_ceremony_rows` owns the mechanics.
        Cmd::MergeCustody { replica, reply } => {
            let r = crate::custody_ceremony_rows::merge_custody(
                local.store,
                local.fleet_plane,
                &replica,
            )
            .await;
            let moved = r.as_ref().is_ok_and(|(_, moved)| *moved);
            return wrote(
                r.map(|(joined, _)| joined),
                |_| moved,
                local.publish_due,
                reply,
            );
        }
        Cmd::PutCustodiesHeld { value, reply } => {
            let r = crate::custody_rows::put_custodies_held(local.fleet_plane, me, &value).await;
            return wrote(r, |_| true, local.publish_due, reply);
        }
        // The offline-share ceremony's write-throughs (store,
        // reached by an app). The two custody kinds ride the SAME fleet
        // plane every other tip-sealed row uses, so the A5 partition and the
        // generation gate judge them without a second door; `group_state_plane`
        // owns the row mechanics. A ceremony must not wait on a nest.
        Cmd::PutGroupHeldRoot { record, reply } => {
            let r = crate::group_state_plane::write_held_root_row(local.fleet_plane, &record).await;
            return wrote(r, |_| true, local.publish_due, reply);
        }
        Cmd::PutGroupReceptionKey { record, reply } => {
            let r =
                crate::group_state_plane::write_reception_key_row(local.fleet_plane, &record).await;
            return wrote(r, |_| true, local.publish_due, reply);
        }
        // Built per call, like every plane here: the scope and its sealing
        // schedule both come out of the one root record, so no caller can
        // pair a scope's rows with another scope's root. `NoFeed` because
        // the group plane's pull transport is the share serve set's (still
        // unwired) — this writes locally only.
        Cmd::AdoptGroupRows { root, rows, reply } => {
            let r = match root.machinery_root() {
                Ok(machinery_root) => {
                    match crate::group_state_plane::GroupStatePlane::new(
                        local.store,
                        &crate::group_state_plane::NoFeed,
                        &machinery_root,
                        local.writer_key,
                        &root.scope_id,
                    ) {
                        Ok(plane) => plane.adopt_rows(&rows).await,
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(anyhow::anyhow!(
                    "the held root record does not yield its machinery root: {e}"
                )),
            };
            return wrote(r, |_| false, local.publish_due, reply);
        }
        Cmd::ResolveFleetRemoval {
            row,
            claimed,
            reply,
        } => {
            let outcome = match crate::fleet_removal::removal_facts(
                local.store,
                local.trust,
                me,
                local.principal_slot.grant_registration_row(),
            )
            .await
            {
                Ok(facts) => fauna_core::fleet_removal::resolve_removal_targets(
                    &facts,
                    &row,
                    claimed.as_ref(),
                ),
                Err(e) => Err(FleetRemovalRefusal::Unavailable(format!("{e:#}"))),
            };
            if let Err(refusal) = &outcome {
                tracing::warn!(
                    "account runtime: fleet removal refused ({refusal:?}) — nothing is deleted"
                );
            }
            let _ = reply.send(outcome);
        }
        Cmd::UnaccountedFleetMembers { roster, reply } => {
            let _ = reply.send(
                crate::fleet_removal::unaccounted_members(
                    local.store,
                    local.trust,
                    me,
                    local.principal_slot.grant_registration_row(),
                    &roster,
                )
                .await,
            );
        }
        Cmd::RemovedDeviceIds { reply } => {
            let _ = reply
                .send(crate::fleet_removal::removed_device_ids(local.store, local.trust).await);
        }
        Cmd::RemoveFleetMember { member, reply } => {
            let targets = match crate::fleet_removal::removal_facts(
                local.store,
                local.trust,
                me,
                local.principal_slot.grant_registration_row(),
            )
            .await
            {
                Ok(facts) => fauna_core::fleet_removal::resolve_member_removal(&facts, &member),
                Err(e) => Err(FleetRemovalRefusal::Unavailable(format!("{e:#}"))),
            };
            let targets = match targets {
                Ok(targets) => targets,
                Err(refusal) => {
                    tracing::warn!(
                        "account runtime: member removal refused ({refusal:?}) — nothing is written"
                    );
                    let _ = reply.send(Err(refusal));
                    return Served::Done;
                }
            };
            // One target by construction (the member itself); an already
            // removed id resolved to none and writes nothing.
            let mut r = Ok(());
            for target in targets {
                r = crate::fleet_removal::write_removed(
                    local.store,
                    local.fleet_plane,
                    local.trust,
                    local.writer_key,
                    target,
                )
                .await;
                if r.is_err() {
                    break;
                }
            }
            // `wrote`'s three duties, over a typed reply: arm the publish,
            // answer, and reassemble on the stale-writer refusal.
            let stale = stale_writer_in(&r);
            if r.is_ok() {
                *local.publish_due = true;
            }
            let _ = reply.send(r.map_err(|e| FleetRemovalRefusal::Unavailable(format!("{e:#}"))));
            if stale {
                tracing::info!(
                    "account runtime: writer rotated under this process — \
                     reassembling to adopt the successor (caller retries)"
                );
                return Served::Reassemble;
            }
        }
        Cmd::StageFleetRemoval { removal, reply } => {
            let _ = reply.send(crate::fleet_removal::stage(
                local.principal_slot,
                &removal.row,
                &removal.targets,
            ));
        }
        Cmd::SettleFleetRemoval {
            removal,
            outcome,
            reply,
        } => {
            let wrote_rows = matches!(outcome, NestDeletion::Gone);
            let r = crate::fleet_removal::settle(
                local.store,
                local.fleet_plane,
                local.trust,
                local.writer_key,
                local.principal_slot,
                &removal,
                outcome,
            )
            .await;
            // A `Gone` settle journals its rows even when it then fails, so
            // the publish is owed either way.
            if wrote_rows {
                *local.publish_due = true;
            }
            return wrote(r, |_| false, local.publish_due, reply);
        }
        Cmd::DataVersion { reply } => {
            let _ = reply.send(local.store.data_version().await);
        }
        Cmd::KeyedPrincipals { reply } => {
            let _ = reply.send(
                crate::generation_tip::keyed_principals_at_tip(
                    local.store,
                    local.trust,
                    local.writer_key,
                    Some(local.principal_slot),
                )
                .await,
            );
        }
        Cmd::GetPreference { kind, reply } => {
            let r = local
                .store
                .state(&kind, fauna_protocol::merge_policy::PREFERENCE_KEY)
                .await;
            let _ = reply.send(r);
        }
        Cmd::StatesOfKind { kind, reply } => {
            let _ = reply.send(local.store.states_of_kind(&kind).await);
        }
        Cmd::DeploymentSeedPublished {
            nest_actor_id,
            reply,
        } => {
            let _ = reply.send(
                crate::deployment_seed_rows::deployment_seed_published(
                    local.store,
                    local.fleet_plane,
                    &nest_actor_id,
                )
                .await,
            );
        }
        Cmd::PutPreference { kind, value, reply } => {
            let r = put_preference_local(local.store, local.plane, &kind, value).await;
            let stale = stale_writer_in(&r);
            if r.is_ok() {
                *local.publish_due = true;
            }
            let _ = reply.send(r);
            if stale {
                tracing::info!(
                    "account runtime: writer rotated under this process — \
                     reassembling to adopt the successor (caller retries)"
                );
                return Served::Reassemble;
            }
        }
        // Recorded, not merged into the walk set here: every pass
        // re-derives (and unions `registered` back in) before it
        // reads the set, so the registration lands on the next one.
        Cmd::RegisterContentScope { scope, reply } => {
            if !local.registered.contains(&scope) {
                local.registered.push(scope);
            }
            let _ = reply.send(());
        }
        Cmd::EnqueueIntent {
            kind,
            scope,
            payload,
            drainer,
            reply,
        } => {
            let r =
                crate::outbox::enqueue_intent(local.store, &kind, &scope, payload, drainer).await;
            let stale = stale_writer_in(&r);
            let _ = reply.send(r);
            if stale {
                tracing::info!(
                    "account runtime: writer rotated under this process — \
                     reassembling to adopt the successor (caller retries)"
                );
                return Served::Reassemble;
            }
        }
        Cmd::PrincipalBundleStatus { reply } => {
            let _ = reply.send(local.principal_slot.status());
        }
        // Resolved on the store thread beside the latch it reads, for the
        // same reason `PrincipalBundleStatus` is: the slot the assembly
        // holds is the one whose grant encoding the latch is
        // content-addressed against, so a reader off-thread could pair a
        // fresh grant with a stale row.
        Cmd::EnrolledDeviceRow { reply } => {
            let _ = reply.send(local.principal_slot.grant_registration_row());
        }
        Cmd::EnrollmentRefusal { reply } => {
            let _ = reply.send(local.principal_slot.enrollment_refusal());
        }
        // The v1 authority seam, resolved on the store thread so the key and
        // the witness that authorizes it always come from the SAME assembly
        // — a caller pairing a freshly rotated writer key with a stale
        // carriage would mint roster entries nothing can verify.
        Cmd::GroupCeremonyAuthority { reply } => {
            let _ = reply.send(local.principal_slot.device_authorization_carriage().map(
                |device_authorization| GroupCeremonyAuthority {
                    device_key: local.writer_key.clone(),
                    device_authorization,
                },
            ));
        }
        // The peer leg's cached brake evidence, re-served to the share
        // plane's bind (rule 7: cached last-known for offline starts). One cache, one owner: the peer
        // leg's pump writes it, this read only re-serves it; a corrupt or
        // absent cache is `None` (no evidence — the refusing arm).
        Cmd::CachedNestCapabilities { reply } => {
            let _ = reply.send(
                crate::host_legs::cached_nest_facts(local.store)
                    .await
                    .map(|facts| facts.capabilities),
            );
        }
        Cmd::P2pParticipation { reply } => {
            let _ = reply.send(crate::p2p_participation::load(local.store).await);
        }
        // The device's own switch: load-modify-save on the store thread, so a
        // pass's fold (`peer_leg::ensure_bound`) and the toggle never interleave
        // inside one row.
        Cmd::SetP2pParticipation { on, reply } => {
            let mut row = crate::p2p_participation::load(local.store).await;
            let r = if row.set_local(on) {
                crate::p2p_participation::save(local.store, &row).await
            } else {
                Ok(())
            };
            let _ = reply.send(r);
        }
        Cmd::GroupScopeStates { scope_id, reply } => {
            let scope = fauna_protocol::scope::GroupScope::new(scope_id).to_string();
            let _ = reply.send(local.store.group_scope_states(&scope).await);
        }
        Cmd::GroupRosterSnapshot { reply } => {
            let _ = reply
                .send(crate::group_scope_view::GroupRosterSnapshot::load_held(local.store).await);
        }
        // Every pass-bound variant is the serve loop's.
        _ => unreachable!(
            "a pass-bound command reached the local server — `Cmd::is_local` and this match \
             disagree"
        ),
    }
    Served::Done
}

/// [`AccountStoreHandle::raise_read_marker`]'s body: join the stored marker
/// of `channel_id_hex` with `through` and write it iff that moved it.
async fn raise_read_marker_local<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    channel_id_hex: &str,
    through: u64,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    use fauna_core::read_marker::{ReadMarker, channel_key};
    let key = channel_key(channel_id_hex);
    let kind = fauna_protocol::merge_policy::KIND_READ_MARKER;
    let mut marker = match store.state(kind, &key).await? {
        Some(entry) if !entry.tombstone => {
            fauna_core::encoding::canonical_decode::<ReadMarker>(&entry.value)
                .context("the stored read marker does not decode")?
        }
        _ => ReadMarker::default(),
    };
    if !marker.raise(through) {
        return Ok(false);
    }
    let value = fauna_core::encoding::canonical_encode(&marker).context("encode read marker")?;
    plane
        .put_local(
            &ItemId {
                kind: kind.to_string(),
                key,
            },
            value.to_vec(),
            None,
        )
        .await
        .context("read marker: plane put")?;
    Ok(true)
}

/// The fauna-native read positions among `entries` of
/// `fauna.state.read-marker` ([`AccountStoreHandle::read_markers`]).
fn read_positions_of(entries: &[StateEntry]) -> Vec<(String, u64)> {
    use fauna_core::read_marker::{ReadMarker, channel_of_key};
    entries
        .iter()
        .filter(|e| !e.tombstone)
        .filter_map(|e| {
            let channel = channel_of_key(&e.key)?;
            let marker = fauna_core::encoding::canonical_decode::<ReadMarker>(&e.value).ok()?;
            Some((channel.to_string(), marker.through))
        })
        .collect()
}

/// The distinct escrow holders among `entries` of `fauna.state.escrow-receipt`
/// ([`AccountStoreHandle::escrow_holders`]): each live receipt's `holder_id`,
/// sorted and deduplicated. A tombstoned or undecodable row names nobody.
fn escrow_holders_of(entries: &[StateEntry]) -> Vec<[u8; 32]> {
    let mut holders: Vec<[u8; 32]> = entries
        .iter()
        .filter(|e| !e.tombstone)
        .filter_map(|e| {
            fauna_core::encoding::canonical_decode::<fauna_core::generation::EscrowReceiptRecord>(
                &e.value,
            )
            .ok()
            .map(|r| r.holder_id)
        })
        .collect();
    holders.sort_unstable();
    holders.dedup();
    holders
}

/// [`AccountStoreHandle::registration_chain_moved`]'s body: queue a
/// reconcile-now whose reply receiver is already dropped (the store loop
/// answers with `let _ =`), without awaiting.
fn wake_chain_carry(cmd: &mpsc::Sender<Cmd>) {
    let (reply, _unread) = oneshot::channel();
    match cmd.try_send(Cmd::ReconcileNow { reply }) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::debug!(
                "chain-carry wake dropped — command channel full; next pass carries it"
            );
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::debug!("chain-carry wake after shutdown — dropped");
        }
    }
}

#[cfg(test)]
mod chain_wake_tests {
    use super::*;

    /// The wake queues exactly one pass-bound reconcile-now.
    #[tokio::test]
    async fn the_chain_wake_queues_a_reconcile_now() {
        let (tx, mut rx) = mpsc::channel(4);
        wake_chain_carry(&tx);
        assert!(matches!(rx.recv().await, Some(Cmd::ReconcileNow { .. })));
    }

    /// A full channel or a gone runtime drops the wake without panicking or
    /// blocking — the next pass is the backstop.
    #[tokio::test]
    async fn a_wake_that_cannot_be_queued_is_dropped() {
        let (tx, rx) = mpsc::channel(1);
        wake_chain_carry(&tx);
        wake_chain_carry(&tx); // full
        drop(rx);
        wake_chain_carry(&tx); // closed
    }
}

#[cfg(test)]
mod escrow_holders_tests {
    use super::*;
    use fauna_core::generation::EscrowReceiptRecord;

    fn receipt(generation: u8, holder: u8, tombstone: bool) -> StateEntry {
        let record = EscrowReceiptRecord {
            generation_id: [generation; 32],
            holder_id: [holder; 32],
            wrap_hash: [0; 32],
            target_key: "identity/00".into(),
            stamped_at_ms: 0,
            holder_sig: vec![],
        };
        StateEntry {
            kind: fauna_protocol::merge_policy::KIND_ESCROW_RECEIPT.into(),
            key: format!("{generation}/{holder}"),
            scope: String::new(),
            value: fauna_core::encoding::canonical_encode(&record).expect("encode"),
            merge_meta: None,
            entry_version: 1,
            tombstone,
        }
    }

    /// Two generations escrowed with one holder name it once; a tombstoned
    /// receipt and an undecodable row name nobody.
    #[test]
    fn escrow_holders_are_the_distinct_live_receipt_holders() {
        let mut junk = receipt(9, 0xCC, false);
        junk.value = vec![0xFF];
        let entries = [
            receipt(1, 0xBB, false),
            receipt(2, 0xBB, false),
            receipt(1, 0xAA, false),
            receipt(3, 0xDD, true),
            junk,
        ];
        assert_eq!(escrow_holders_of(&entries), vec![[0xAA; 32], [0xBB; 32]]);
    }
}
