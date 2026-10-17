//! The **consent-time grant to a third-party principal** over its `ext.*`
//! kinds (`docs/goal/architecture/third-party-kinds.md` § The record doors →
//! *Who mints the consent-time grant*): the one shared-Rust entry point every
//! app's approve path calls, so none re-implements a step.
//!
//! [`prepare_ext_consent_grant`] runs, in order: verify the request's manifest
//! JWS against the `client_id`'s host; decide which kinds the request's
//! `fauna:records:rw:<qualifier>` scopes cover ([`plan_ext_consent`]); publish
//! the `fauna.state.kind-manifest` row (the account's overlay admits the kinds
//! from it); derive each kind's delegable pair and build the grant
//! ([`crate::mint_ext_kinds_grant`]); sign its `Mint` event and record it in
//! the grant log — and only then release the blob bytes. The caller deposits
//! them (`fauna.capabilities.mint`) and resolves the consent, in that order:
//! the principal row is minted nest-side at `/oauth/token` with no client in
//! the loop, so the grant must already rest when it is.
//!
//! The same entry point is the home of every **twin** a consent's scopes
//! name (`third-party-kinds.md` § *Who mints the consent-time grant*, ruled
//! 2026-10-05): a `fauna:folder:read:<id>` scope plans the folder read twin —
//! `content.read{folder, set}`, one wrap per content-key generation from the
//! owner's custody (`webdav-server.md` § Key model → *A principal's read*
//! rule (1)) — as its OWN grant beside the consent's one records grant: one
//! grant per (principal, folder), under the derived id
//! [`crate::folder_principal_grant_id`] at the generation the owner's log
//! walks to ([`crate::folder_principal_generation`]), with its own `Mint`
//! event. The twin has a lifecycle the records grant does not (renewed on its
//! set's rotation, revoked on its unserve, rule (4)), and the derived id is
//! the owner's only handle on it. The caller resolves each named folder from
//! the owner's custody first ([`ConsentFolder`]); the twin is mintable only
//! over a set that custody marks served.
//!
//! The `fauna:personalization:rw` scope's twin (`authorization-server.md`
//! § Scope grammar → *The fifth arm*) rides the consent's one grant beside
//! the records tuples, under the caller's id: two `content.read` tuples, the
//! delegable pairs of `fauna.state.personalization` and
//! `fauna.personalization.model` ([`crate::personalization_scope_wraps`]) —
//! no manifest, and refused with [`ExtConsentError::NoHolderKey`] when the
//! ceremony attested no key to wrap to.
//!
//! A `fauna:folder:deposit:<id>` scope's twin (`file-sync.md` § Third-party
//! deposit ingress) rides the consent's one grant too: one keyless `deposit`
//! tuple per folder ([`crate::folder_deposit_scope_wraps`]), wrapping nothing
//! — the audit + revocation record the nest's deposit door re-resolves at
//! every deposit. The account's ownership of the folder is the nest's check;
//! here a deposit needs only an attested holder key to name.
//!
//! A bare folder-plane scope (`fauna:folder:deposit`, `fauna:folder:read` —
//! the folder plane's qualifier is the user's, `authorization-server.md`
//! § Scope grammar) names no folder until the card's choice qualifies it:
//! the caller qualifies the row with [`approved_scopes`] first, and a bare
//! scope that still reaches the planner is refused
//! ([`ExtConsentError::FolderUnchosen`]), never resolved keyless.
//!
//! An oracle scope (`fauna:identity:op:<class>`, `key-material-hierarchy.md`
//! § Audience: deployment infrastructure → *The oracle*) declares one
//! keyless `identity.op` tuple per named class ([`identity_op_classes`],
//! [`crate::identity_op_scope`]) in that same one grant, under the caller's
//! id. The tuple wraps no key: the class's custodian performs it against
//! the live grant row, and the principal's revoke ends it with the rest of
//! the grant.
//!
//! A request carrying no twin-bearing scope has nothing to mint here (an
//! empty list), and the caller resolves it as it always has.

use std::collections::BTreeMap;

use fauna_core::crypto::DelegableSchedule;
use fauna_core::ext_kind::{ExtKind, ExtQualifier};
use fauna_mls::wrapped_blob::{GrantWindow, ScopeWraps, build_grant_blob_with_epochs};
use fauna_protocol::atproto_pds::PendingConsentRow;
use fauna_protocol::kind_manifest::{VerifiedManifest, client_id_host, verify_manifest};
use fauna_protocol::merge_policy::AdmittedKinds;

use crate::grant_log::{self, GrantEventSigner};
use crate::{
    DEFAULT_GRANT_WINDOW_SECS, MintGrantError, ext_kinds_scope_wraps, folder_deposit_scope_wraps,
    folder_principal_generation, folder_principal_grant_id, folder_read_scope_wraps,
    personalization_scope_wraps,
};

/// What a records consent will grant, decided before anything is written.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtConsentPlan {
    /// The document's manifest, verified against the `client_id`'s host.
    pub manifest: VerifiedManifest,
    /// Every kind the request's `records` scopes cover, sorted, no duplicates
    /// — one `content.read` tuple each (and one `content.write` tuple each
    /// when [`Self::writer`] is set).
    pub kinds: Vec<ExtKind>,
    /// The attested X25519 holder the grant is wrapped to.
    pub holder: [u8; 32],
    /// The attested Ed25519 writer — `None` → the principal is read-only
    /// over its kinds, and the grant carries no `content.write` tuple.
    pub writer: Option<[u8; 32]>,
}

/// Why a records consent cannot be granted as requested. Every arm is a
/// reason to leave the request unanswered (the user may decline it); none is
/// retried silently.
#[derive(Debug, thiserror::Error)]
pub enum ExtConsentError {
    /// The ceremony attested no holder key, so nothing can be wrapped to it.
    #[error("the app attested no key to receive the records' keys")]
    NoHolderKey,
    /// The request names records but the document carries no manifest.
    #[error("the app's document declares no record kinds")]
    NoManifest,
    /// The manifest did not verify against the `client_id`'s host.
    #[error("the app's kind manifest is refused: {0}")]
    ManifestRefused(String),
    /// A single kind of the app's own publisher its manifest does not declare.
    #[error("{0} is not declared by the app's manifest")]
    KindNotDeclared(String),
    /// A single kind of another publisher this account has not admitted
    /// through that publisher's own manifest — not yet known on this account.
    #[error("{0} is not yet known on this account")]
    KindNotAdmitted(String),
    /// The request's records scopes cover no kind at all.
    #[error("the request covers no record kind")]
    NothingToGrant,
    /// The account store refused a read or the manifest row's write.
    #[error(transparent)]
    Store(#[from] fauna_client_config::StoreError),
    /// The grant could not be built (a malformed holder key).
    #[error(transparent)]
    Mint(#[from] MintGrantError),
    /// The blob could not be encoded.
    #[error("grant encoding: {0}")]
    Encode(String),
    /// A `folder:read` scope names a folder the owner's custody does not
    /// resolve — not one of this account's own folders, or unreadable here.
    #[error("folder {0} is not one of your folders")]
    FolderUnknown(i64),
    /// A `folder:read` scope names a folder the owner does not serve over
    /// WebDAV: an unserved set rests under keys no grant may wrap, and serving
    /// is the owner's own act, never a consent tap's (`webdav-server.md`
    /// § Key model → *A principal's read* (2)).
    #[error(
        "\"{0}\" is not served over WebDAV — serve it from its folder settings before an app \
         can read it"
    )]
    FolderNotServed(String),
    /// A bare folder-plane scope with no folder chosen for it — the card's
    /// choice qualifies it (`authorization-server.md` § Consent → *The card
    /// chooses the folder*).
    #[error("choose the folder this app may use")]
    FolderUnchosen,
    /// A folder chosen for a request naming no bare folder-plane scope —
    /// nothing takes the choice, and the nest would refuse it.
    #[error("this request takes no folder choice")]
    FolderChoiceNotTaken,
    /// A served folder whose content keys this device's custody does not hold
    /// — nothing to wrap, so nothing is granted (fail closed).
    #[error("this device holds no keys for \"{0}\"")]
    FolderNoKeys(String),
    /// The owner's signer refused the `Mint` event.
    #[error(transparent)]
    Sign(#[from] grant_log::GrantEventSignError),
    /// The ledger write did not keep the `Mint` event, so the blob stays put.
    #[error(transparent)]
    Unrecorded(#[from] grant_log::UnrecordedGrantError),
}

/// One folder a `folder:read` scope names, as the owner's custody resolves it
/// — the consent machine's folder seam answers it (`webdav-server.md` § Key
/// model → *A principal's read* (1)).
#[derive(Clone)]
pub struct ConsentFolder {
    /// The set's name — the twin's `set` is its hash; the card names it.
    pub set_name: String,
    /// Whether the owner's custody marks the set WebDAV-served (ruling
    /// (7)(b)(ii)) — never the nest's `webdav_enabled` flag.
    pub served_by_custody: bool,
    /// The custody channel the set's content keys rest at; `None` when it
    /// cannot be resolved (a malformed group id).
    pub custody_channel_id: Option<[u8; 32]>,
    /// The owner's folder-key custody the wraps are cut from.
    pub custody: fauna_core::data::FoldersConfig,
}

/// Whether approving a consent naming `scope` mints a twin — a `records`, a
/// `folder:read`, a `folder:deposit`, the `personalization:rw` or an
/// `identity:op` scope. A bare folder-plane scope names none until
/// [`approved_scopes`] qualifies it.
#[must_use]
pub fn names_twin_scope(scope: &str) -> bool {
    fauna_bridge_atproto::fauna_scope::records_qualifier(scope).is_some()
        || fauna_bridge_atproto::fauna_scope::folder_read_qualifier(scope).is_some()
        || fauna_bridge_atproto::fauna_scope::folder_deposit_qualifier(scope).is_some()
        || names_personalization(scope)
        || fauna_bridge_atproto::fauna_scope::identity_op_qualifier(scope).is_some()
}

/// A consent row's `scopes` as an approve grants them: every bare
/// folder-plane scope qualified by the card's chosen `folder`
/// (`fauna_scope::qualify_bare_scopes`, the string the nest records), every
/// other scope untouched — what the caller hands
/// [`prepare_ext_consent_grant`] and resolves with. A row carrying no bare
/// scope takes no choice.
///
/// # Errors
/// [`ExtConsentError::FolderUnchosen`] (a bare scope, no choice),
/// [`ExtConsentError::FolderChoiceNotTaken`] (a choice, no bare scope),
/// [`ExtConsentError::FolderUnknown`] (a choice that is no folder id).
pub fn approved_scopes(
    scopes: &[String],
    folder: Option<i64>,
) -> Result<Vec<String>, ExtConsentError> {
    let bare = scopes
        .iter()
        .any(|s| fauna_bridge_atproto::fauna_scope::user_qualified_bare_arm(s).is_some());
    match (bare, folder) {
        (false, None) => Ok(scopes.to_vec()),
        (false, Some(_)) => Err(ExtConsentError::FolderChoiceNotTaken),
        (true, None) => Err(ExtConsentError::FolderUnchosen),
        (true, Some(id)) => fauna_bridge_atproto::fauna_scope::qualify_bare_scopes(scopes, id)
            .ok_or(ExtConsentError::FolderUnknown(id)),
    }
}

/// The folder ids `scopes`' `folder:deposit` scopes name, sorted, no
/// duplicates — one keyless `deposit` tuple each in the consent's grant.
#[must_use]
pub fn folder_deposit_ids(scopes: &[String]) -> Vec<i64> {
    let mut ids: Vec<i64> = scopes
        .iter()
        .filter_map(|s| fauna_bridge_atproto::fauna_scope::folder_deposit_qualifier(s))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Whether `scope` is the `fauna:personalization:rw` arm — whose twin, the
/// registry kind's and the model kind's delegable pairs, rides the consent's
/// one grant ([`crate::personalization_scope_wraps`]).
#[must_use]
pub fn names_personalization(scope: &str) -> bool {
    fauna_bridge_atproto::fauna_scope::arm_of(scope)
        == Some(fauna_bridge_atproto::fauna_scope::FaunaScopeArm::PersonalizationRw)
}

/// The oracle classes `scopes`' `fauna:identity:op:<class>` scopes name,
/// in table order, no duplicates — each one keyless `identity.op` tuple the
/// consent's grant declares ([`crate::identity_op_scope`]).
#[must_use]
pub fn identity_op_classes(scopes: &[String]) -> Vec<fauna_core::identity_op::IdentityOpClass> {
    fauna_core::identity_op::IdentityOpClass::ALL
        .iter()
        .copied()
        .filter(|class| {
            scopes.iter().any(|s| {
                fauna_bridge_atproto::fauna_scope::identity_op_qualifier(s) == Some(*class)
            })
        })
        .collect()
}

/// The folder ids `scopes`' `folder:read` scopes name, sorted, no duplicates —
/// what the caller resolves through its folder seam before
/// [`prepare_ext_consent_grant`].
#[must_use]
pub fn folder_read_ids(scopes: &[String]) -> Vec<i64> {
    let mut ids: Vec<i64> = scopes
        .iter()
        .filter_map(|s| fauna_bridge_atproto::fauna_scope::folder_read_qualifier(s))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The folder read twins `ids` plan to: one `content.read{folder, set}` tuple
/// per folder, its wraps every content-key generation custody holds. Refuses
/// the whole consent on the first folder that is unresolved, unserved, or
/// keyless — the user may still decline it.
///
/// # Errors
/// [`ExtConsentError::FolderUnknown`], [`ExtConsentError::FolderNotServed`],
/// [`ExtConsentError::FolderNoKeys`].
pub fn plan_folder_reads(
    ids: &[i64],
    folders: &BTreeMap<i64, ConsentFolder>,
) -> Result<Vec<ScopeWraps>, ExtConsentError> {
    ids.iter()
        .map(|id| {
            let folder = folders.get(id).ok_or(ExtConsentError::FolderUnknown(*id))?;
            if !folder.served_by_custody {
                return Err(ExtConsentError::FolderNotServed(folder.set_name.clone()));
            }
            let channel = folder
                .custody_channel_id
                .ok_or_else(|| ExtConsentError::FolderNoKeys(folder.set_name.clone()))?;
            folder_read_scope_wraps(&folder.custody, &folder.set_name, &channel)
                .map_err(|_| ExtConsentError::FolderNoKeys(folder.set_name.clone()))
        })
        .collect()
}

/// Decide what `consent` grants: `Ok(None)` when it names no `records` scope.
///
/// A wildcard (`ext.<publisher>.*`, its publisher held to the document's host
/// at PAR) covers the manifest's declared kinds of that publisher — structural
/// equality, never a string prefix. A single kind of the app's own publisher
/// must be one the manifest declares; a single **foreign** kind is grantable
/// only if `admitted` (the account's overlay) already holds it through its own
/// publisher's manifest.
///
/// # Errors
/// Every [`ExtConsentError`] arm up to [`ExtConsentError::NothingToGrant`].
pub fn plan_ext_consent(
    consent: &PendingConsentRow,
    admitted: &AdmittedKinds,
) -> Result<Option<ExtConsentPlan>, ExtConsentError> {
    let qualifiers: Vec<ExtQualifier> = consent
        .scopes
        .iter()
        .filter_map(|s| fauna_bridge_atproto::fauna_scope::records_qualifier(s))
        .collect();
    if qualifiers.is_empty() {
        return Ok(None);
    }
    let holder = key32(consent.holder_x25519.as_deref().map(Vec::as_slice))
        .ok_or(ExtConsentError::NoHolderKey)?;
    let writer = key32(consent.writer_ed25519.as_deref().map(Vec::as_slice));
    let jws = consent
        .fauna_manifest
        .as_deref()
        .ok_or(ExtConsentError::NoManifest)?;
    let host = client_id_host(&consent.client_id).ok_or_else(|| {
        ExtConsentError::ManifestRefused("the document is not served over https".into())
    })?;
    let manifest =
        verify_manifest(jws, &host).map_err(|e| ExtConsentError::ManifestRefused(e.to_string()))?;

    let mut kinds = Vec::new();
    for qualifier in &qualifiers {
        match qualifier {
            ExtQualifier::Kind(kind) if kind.publisher() == manifest.publisher_domain => {
                if !manifest.kinds.iter().any(|k| &k.kind == kind) {
                    return Err(ExtConsentError::KindNotDeclared(kind.to_string()));
                }
                kinds.push(kind.clone());
            }
            ExtQualifier::Kind(kind) => {
                if !admitted.contains(kind) {
                    return Err(ExtConsentError::KindNotAdmitted(kind.to_string()));
                }
                kinds.push(kind.clone());
            }
            wildcard => kinds.extend(
                manifest
                    .kinds
                    .iter()
                    .filter(|k| wildcard.covers(&k.kind))
                    .map(|k| k.kind.clone()),
            ),
        }
    }
    kinds.sort();
    kinds.dedup();
    if kinds.is_empty() {
        return Err(ExtConsentError::NothingToGrant);
    }
    Ok(Some(ExtConsentPlan {
        manifest,
        kinds,
        holder,
        writer,
    }))
}

/// One recorded, not-yet-deposited grant of a consent: the caller hands
/// [`Self::blob_bytes`] to `fauna.capabilities.mint`, then resolves.
#[derive(Debug)]
pub struct PreparedExtGrant {
    /// The grant's id, as its `Mint` event names it — the caller's id for the
    /// records grant, the derived [`folder_principal_grant_id`] for a folder
    /// twin.
    pub grant_id: [u8; 16],
    /// The canonical `GrantBlob` bytes — released only against a log that
    /// holds the `Mint` event.
    pub blob_bytes: Vec<u8>,
    /// The kinds the grant covers (the records grant's; empty on a twin).
    pub kinds: Vec<ExtKind>,
    /// The folders whose read twin the grant carries — exactly one on a
    /// folder twin, none on the records grant.
    pub folders: Vec<i64>,
    /// Whether this mint replaces a live grant in place under its own id (the
    /// generation walk found the principal already reading the set) rather
    /// than opening a fresh generation. A withdrawn approve revokes only the
    /// fresh ones (`webdav-server.md` § Key model → *A principal's read*
    /// rule (1), *A withdrawn approve ends only what it began*).
    pub replaced: bool,
}

/// The owner-side inputs of [`prepare_ext_consent_grant`].
pub struct ExtConsentOwner<'a> {
    /// The owner's identity pubkey (the blob's index, the log's chain).
    pub actor_id: [u8; 32],
    /// The owner's identity secret — the input of every folder twin's derived
    /// id ([`folder_principal_grant_id`]). A field, not a derivation closure:
    /// the id must be the one rule (4)'s renew and revoke recompute from the
    /// same secret, so the derivation is spelled at its one call site, and a
    /// closure would narrow no custody — the seams already hold the keypair
    /// the [`Self::signer`] signs with.
    pub owner_secret: &'a [u8; 32],
    /// The owner's delegable branch — the only typed path a grant pair leaves.
    pub delegable: &'a DelegableSchedule,
    /// The account's kind-manifest rows.
    pub manifests: &'a dyn fauna_client_config::KindManifestStore,
    /// The account's grant-event log.
    pub ledger: &'a dyn fauna_client_config::SuccessionLedgerStore,
    /// The owner's grant-event signer.
    pub signer: &'a dyn GrantEventSigner,
}

/// One grant planned for a consent, before it is built.
struct PlannedGrant {
    grant_id: [u8; 16],
    scopes: Vec<ScopeWraps>,
    kinds: Vec<ExtKind>,
    folders: Vec<i64>,
    replaced: bool,
}

/// Steps (1)–(3) of the consent-time mint for `consent`, ending at the grants
/// the caller may deposit, in order: the consent's one grant first (under the
/// caller's `grant_id` — the records tuples, the personalization twin and the
/// keyless `deposit` tuples), then one grant per `folder:read` scope in
/// folder-id order (each under its derived id) — empty when the request names
/// no twin-bearing scope, and a folder-read-only consent mints no consent
/// grant. `consent`'s scopes are the approve's ([`approved_scopes`]); a bare
/// folder-plane scope left in them refuses.
/// `folders` is the caller's custody resolution of every id
/// [`folder_read_ids`] names (a missing one refuses). `grant_id` and
/// `now_secs` come from the caller (this crate has no clock or entropy of its
/// own); every window is [`DEFAULT_GRANT_WINDOW_SECS`].
///
/// Every check runs before anything is written: the folder twins are planned
/// first, then the manifest row is published (a grant whose kinds the
/// account's replicas cannot admit would deposit keys to rows nobody can
/// merge), then each folder's generation is walked over the stored log
/// ([`folder_principal_generation`]) and its id derived, then every grant's
/// `Mint` event is signed and joined into the log in ONE merge through the
/// grant-mint door, and each blob is released only against the log that write
/// actually stored and the bound nest acknowledged
/// ([`grant_log::UndepositedGrant::release`]) — record, publish, then deposit,
/// as every grant this app mints. A replaced twin's id already carries a `Mint`, so its
/// release check would pass even had its new event been lost; the single
/// merge, which keeps all of the events or none, is what makes that moot.
///
/// # Errors
/// [`plan_ext_consent`]'s and [`plan_folder_reads`]'s, a store refusal, or a
/// mint, sign or record failure — each leaves nothing deposited.
pub async fn prepare_ext_consent_grant(
    owner: &ExtConsentOwner<'_>,
    consent: &PendingConsentRow,
    folders: &BTreeMap<i64, ConsentFolder>,
    grant_id: [u8; 16],
    now_secs: u64,
) -> Result<Vec<PreparedExtGrant>, ExtConsentError> {
    if consent
        .scopes
        .iter()
        .any(|s| fauna_bridge_atproto::fauna_scope::user_qualified_bare_arm(s).is_some())
    {
        return Err(ExtConsentError::FolderUnchosen);
    }
    let names_records = consent
        .scopes
        .iter()
        .any(|s| fauna_bridge_atproto::fauna_scope::records_qualifier(s).is_some());
    let records = if names_records {
        let admitted = owner.manifests.admitted_kinds().await?;
        plan_ext_consent(consent, &admitted)?
    } else {
        None
    };
    let folder_ids = folder_read_ids(&consent.scopes);
    let personalization = consent.scopes.iter().any(|s| names_personalization(s));
    let deposit_ids = folder_deposit_ids(&consent.scopes);
    let op_scopes: Vec<ScopeWraps> = identity_op_classes(&consent.scopes)
        .into_iter()
        .map(|class| (crate::identity_op_scope(class), Vec::new()))
        .collect();
    if records.is_none()
        && folder_ids.is_empty()
        && !personalization
        && deposit_ids.is_empty()
        && op_scopes.is_empty()
    {
        return Ok(Vec::new());
    }
    let holder = match &records {
        Some(plan) => plan.holder,
        None => key32(consent.holder_x25519.as_deref().map(Vec::as_slice))
            .ok_or(ExtConsentError::NoHolderKey)?,
    };
    let twins = plan_folder_reads(&folder_ids, folders)?;

    let mut planned = Vec::with_capacity(1 + twins.len());
    // The consent's one grant, under the caller's id: the records tuples, the
    // personalization twin and the keyless deposit tuples share a lifecycle —
    // each ends with the principal — so they ride one blob and one `Mint`.
    let mut consent_scopes = Vec::new();
    let mut consent_kinds = Vec::new();
    if let Some(plan) = records {
        let now_ms = i64::try_from(now_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
        owner
            .manifests
            .publish(&consent.client_id, &plan.manifest, now_ms)
            .await?;
        consent_scopes = ext_kinds_scope_wraps(owner.delegable, &plan.kinds, plan.writer.as_ref());
        consent_kinds = plan.kinds;
    }
    if personalization {
        consent_scopes.extend(personalization_scope_wraps(owner.delegable));
    }
    consent_scopes.extend(folder_deposit_scope_wraps(&deposit_ids));
    // The oracle's keyless tuples end with the principal too.
    consent_scopes.extend(op_scopes);
    if !consent_scopes.is_empty() {
        planned.push(PlannedGrant {
            grant_id,
            scopes: consent_scopes,
            kinds: consent_kinds,
            folders: Vec::new(),
            replaced: false,
        });
    }
    if !twins.is_empty() {
        let log = owner.ledger.load().await?.grant_events;
        for (id, twin) in folder_ids.iter().zip(twins) {
            // `plan_folder_reads` resolved every id, so the lookup holds.
            let set_name = &folders[id].set_name;
            let walk = folder_principal_generation(&log, owner.owner_secret, &holder, set_name);
            planned.push(PlannedGrant {
                grant_id: folder_principal_grant_id(
                    owner.owner_secret,
                    &holder,
                    set_name,
                    walk.generation,
                ),
                scopes: vec![twin],
                kinds: Vec::new(),
                folders: vec![*id],
                replaced: walk.live.is_some(),
            });
        }
    }

    let window_end = now_secs.saturating_add(DEFAULT_GRANT_WINDOW_SECS);
    let mut pending = Vec::with_capacity(planned.len());
    let mut signed = Vec::with_capacity(planned.len());
    for grant in planned {
        let blob = build_grant_blob_with_epochs(
            &owner.actor_id,
            &grant.grant_id,
            &holder,
            None,
            GrantWindow(now_secs, window_end),
            &grant.scopes,
        )
        .map_err(MintGrantError::from)?;
        let bytes = blob
            .to_canonical_bytes()
            .map_err(|e| ExtConsentError::Encode(e.to_string()))?;
        signed.push(owner.signer.sign_grant_event(grant_log::build_mint_event(
            grant.grant_id,
            holder,
            grant_log::event_scope_of(&blob.scope),
            now_secs,
            window_end,
            now_secs,
        ))?);
        pending.push((
            grant_log::UndepositedGrant::new(grant.grant_id, bytes),
            grant,
        ));
    }
    let published = owner
        .ledger
        .merge_published(
            fauna_core::succession_ledger::SuccessionLedger::events_replica(
                fauna_core::identity::ActorId(owner.actor_id),
                signed,
            ),
        )
        .await?;
    let recorded = grant_log::PublishedGrants::from_published(&published);
    pending
        .into_iter()
        .map(|(undeposited, grant)| {
            Ok(PreparedExtGrant {
                grant_id: grant.grant_id,
                blob_bytes: undeposited.release(&recorded)?,
                kinds: grant.kinds,
                folders: grant.folders,
                replaced: grant.replaced,
            })
        })
        .collect()
}

fn key32(bytes: Option<&[u8]>) -> Option<[u8; 32]> {
    bytes.and_then(|b| b.try_into().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_config::KindManifestStore as _;
    use fauna_client_config::test_helpers::{FakeKindManifestStore, FakeSuccessionLedgerStore};
    use fauna_core::crypto::BackupKey;
    use fauna_core::identity::ActorKeypair;
    use fauna_mls::wrapped_blob::{GrantBlob, ScopeTuple, generate_x25519_keypair};

    const CLIENT: &str = "https://app.example/client-metadata.json";

    struct KeypairSigner(ActorKeypair);

    impl GrantEventSigner for KeypairSigner {
        fn sign_grant_event(
            &self,
            event: fauna_core::grant_event::GrantEvent,
        ) -> Result<fauna_core::grant_event::GrantEvent, grant_log::GrantEventSignError> {
            event
                .sign(self.0.signing_key())
                .map_err(|e| grant_log::GrantEventSignError::Sign(e.to_string()))
        }
    }

    fn manifest(domain: &str, kinds: &[&str]) -> String {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let did = fauna_protocol::kind_manifest::ed25519_did_key(&key.verifying_key().to_bytes());
        let payload = serde_json::json!({
            "version": 1,
            "publisher": { "domain": domain, "key": did },
            "kinds": kinds.iter().map(|k| serde_json::json!({
                "kind": k, "class": "state", "merge": "latest-wins", "floor": "none"
            })).collect::<Vec<_>>(),
        });
        fauna_protocol::kind_manifest::sign_manifest(&key, &payload, None)
    }

    fn consent(scopes: &[&str], holder: [u8; 32], writer: Option<[u8; 32]>) -> PendingConsentRow {
        PendingConsentRow {
            consent_id: vec![1; 16],
            client_id: CLIENT.into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            holder_x25519: Some(holder.to_vec().into()),
            writer_ed25519: writer.map(|w| w.to_vec().into()),
            fauna_manifest: Some(manifest(
                "app.example",
                &["ext.app.example.notes", "ext.app.example.todo"],
            )),
            ..Default::default()
        }
    }

    fn kinds(plan: &ExtConsentPlan) -> Vec<String> {
        plan.kinds.iter().map(ToString::to_string).collect()
    }

    /// No `records` scope → nothing to mint; the caller resolves as before.
    #[test]
    fn a_consent_naming_no_records_scope_plans_nothing() {
        let c = consent(&["fauna:feed:read", "openid"], [1; 32], None);
        assert!(
            plan_ext_consent(&c, &AdmittedKinds::new())
                .unwrap()
                .is_none()
        );
    }

    /// The wildcard covers exactly the manifest's declared kinds of that
    /// publisher, and the attested keys carry through.
    #[test]
    fn a_wildcard_expands_to_the_manifests_declared_kinds() {
        let c = consent(
            &["fauna:records:rw:ext.app.example.*"],
            [1; 32],
            Some([2; 32]),
        );
        let plan = plan_ext_consent(&c, &AdmittedKinds::new())
            .unwrap()
            .unwrap();
        assert_eq!(
            kinds(&plan),
            ["ext.app.example.notes", "ext.app.example.todo"]
        );
        assert_eq!(plan.holder, [1; 32]);
        assert_eq!(plan.writer, Some([2; 32]));
    }

    /// A single own-publisher kind must be declared; a single foreign kind
    /// must already be admitted on this account.
    #[test]
    fn single_kinds_need_a_declaration_or_a_prior_admission() {
        let undeclared = consent(&["fauna:records:rw:ext.app.example.other"], [1; 32], None);
        assert!(matches!(
            plan_ext_consent(&undeclared, &AdmittedKinds::new()),
            Err(ExtConsentError::KindNotDeclared(k)) if k == "ext.app.example.other"
        ));

        let foreign = consent(&["fauna:records:rw:ext.other.org.thing"], [1; 32], None);
        assert!(matches!(
            plan_ext_consent(&foreign, &AdmittedKinds::new()),
            Err(ExtConsentError::KindNotAdmitted(k)) if k == "ext.other.org.thing"
        ));
        let mut admitted = AdmittedKinds::new();
        admitted
            .admit(
                "ext.other.org.thing".parse().unwrap(),
                fauna_protocol::merge_policy::MergePolicy::LatestWins,
            )
            .unwrap();
        let plan = plan_ext_consent(&foreign, &admitted).unwrap().unwrap();
        assert_eq!(kinds(&plan), ["ext.other.org.thing"]);
    }

    /// The refusals that leave nothing to wrap to, or nothing verified.
    #[test]
    fn missing_keys_and_unverified_manifests_refuse() {
        let mut c = consent(&["fauna:records:rw:ext.app.example.*"], [1; 32], None);
        c.holder_x25519 = None;
        assert!(matches!(
            plan_ext_consent(&c, &AdmittedKinds::new()),
            Err(ExtConsentError::NoHolderKey)
        ));

        let mut c = consent(&["fauna:records:rw:ext.app.example.*"], [1; 32], None);
        c.fauna_manifest = None;
        assert!(matches!(
            plan_ext_consent(&c, &AdmittedKinds::new()),
            Err(ExtConsentError::NoManifest)
        ));

        // A manifest signed for another host than the document's.
        let mut c = consent(&["fauna:records:rw:ext.app.example.*"], [1; 32], None);
        c.fauna_manifest = Some(manifest("evil.example", &["ext.evil.example.notes"]));
        assert!(matches!(
            plan_ext_consent(&c, &AdmittedKinds::new()),
            Err(ExtConsentError::ManifestRefused(_))
        ));
    }

    /// **The entry point, end to end:** the manifest row is published, the
    /// `Mint` event lands in the log carrying each kind's writer factor (the
    /// replica-side admission's whole input), and the released blob is the
    /// grant the holder opens — exactly the declared kinds' pairs.
    #[tokio::test]
    async fn the_consent_grant_publishes_records_then_releases_the_blob() {
        let owner_kp = ActorKeypair::generate();
        let owner = owner_kp.actor_id();
        let delegable = DelegableSchedule::derive(&BackupKey::derive(&[7u8; 32]));
        let manifests = FakeKindManifestStore::empty();
        let ledger = FakeSuccessionLedgerStore::empty(owner);
        let signer = KeypairSigner(ActorKeypair::from_secret(*owner_kp.secret_bytes()));
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let writer = [0x5A; 32];
        let c = consent(
            &["fauna:records:rw:ext.app.example.*"],
            holder_pk,
            Some(writer),
        );

        let prepared = prepare_ext_consent_grant(
            &ExtConsentOwner {
                actor_id: owner.0,
                owner_secret: owner_kp.secret_bytes(),
                delegable: &delegable,
                manifests: &manifests,
                ledger: &ledger,
                signer: &signer,
            },
            &c,
            &BTreeMap::new(),
            [9; 16],
            1_000,
        )
        .await
        .unwrap();
        assert_eq!(prepared.len(), 1, "records alone mint one grant");
        let prepared = &prepared[0];
        assert!(!prepared.replaced);

        assert!(
            manifests.row(CLIENT).is_some(),
            "the manifest row is published"
        );
        assert!(
            manifests
                .admitted_kinds()
                .await
                .unwrap()
                .contains(&"ext.app.example.todo".parse().unwrap())
        );
        let events = ledger.current().grant_events;
        assert_eq!(
            events.len(),
            1,
            "the Mint is recorded before the blob leaves"
        );
        let authorized = fauna_core::grant_event::content_write_authorizations(&events);
        for kind in ["ext.app.example.notes", "ext.app.example.todo"] {
            assert_eq!(
                authorized.get(kind).map(|w| w.contains(&writer)),
                Some(true),
                "{kind}: the log authorizes the attested writer"
            );
        }

        let blob = GrantBlob::from_canonical_bytes(&prepared.blob_bytes).unwrap();
        assert_eq!(prepared.grant_id, [9; 16]);
        assert!(
            blob.scope
                .iter()
                .filter(|t| t.class == ScopeTuple::CLASS_CONTENT_WRITE)
                .count()
                == 2
        );
        let opened = crate::open_ext_kind_keys(&blob, &holder_sk).unwrap();
        assert_eq!(opened.len(), 2, "the holder opens both declared kinds");
    }

    /// Record, publish, then deposit (`ui/nests.md` § Trust facet — grants →
    /// *Record-then-deposit*, the published form): a consent `Mint` the bound
    /// nest did not acknowledge releases no blob — a sibling replica could not
    /// yet read the event its reconcile sweep judges the row by.
    #[tokio::test]
    async fn an_unpublished_consent_mint_releases_no_blob() {
        let owner_kp = ActorKeypair::generate();
        let owner = owner_kp.actor_id();
        let delegable = DelegableSchedule::derive(&BackupKey::derive(&[7u8; 32]));
        let manifests = FakeKindManifestStore::empty();
        let ledger = FakeSuccessionLedgerStore::empty(owner);
        ledger.publish_refuses(true);
        let signer = KeypairSigner(ActorKeypair::from_secret(*owner_kp.secret_bytes()));
        let (_holder_sk, holder_pk) = generate_x25519_keypair();
        let c = consent(
            &["fauna:records:rw:ext.app.example.*"],
            holder_pk,
            Some([0x5A; 32]),
        );

        let refused = prepare_ext_consent_grant(
            &ExtConsentOwner {
                actor_id: owner.0,
                owner_secret: owner_kp.secret_bytes(),
                delegable: &delegable,
                manifests: &manifests,
                ledger: &ledger,
                signer: &signer,
            },
            &c,
            &BTreeMap::new(),
            [9; 16],
            1_000,
        )
        .await;

        assert!(
            refused.is_err(),
            "no blob without the nest's acknowledgement"
        );
        assert_eq!(
            ledger.current().grant_events.len(),
            1,
            "the Mint stays recorded locally"
        );
    }

    /// A refused manifest-row write deposits nothing and logs nothing — the
    /// account's replicas could not admit the kinds the grant would key.
    #[tokio::test]
    async fn a_refused_row_write_logs_and_releases_nothing() {
        let owner_kp = ActorKeypair::generate();
        let owner = owner_kp.actor_id();
        let delegable = DelegableSchedule::derive(&BackupKey::derive(&[7u8; 32]));
        let manifests = FakeKindManifestStore::empty();
        manifests.refuse_next_publishes(1);
        let ledger = FakeSuccessionLedgerStore::empty(owner);
        let signer = KeypairSigner(ActorKeypair::from_secret(*owner_kp.secret_bytes()));
        let c = consent(&["fauna:records:rw:ext.app.example.*"], [1; 32], None);
        let out = prepare_ext_consent_grant(
            &ExtConsentOwner {
                actor_id: owner.0,
                owner_secret: owner_kp.secret_bytes(),
                delegable: &delegable,
                manifests: &manifests,
                ledger: &ledger,
                signer: &signer,
            },
            &c,
            &BTreeMap::new(),
            [9; 16],
            1_000,
        )
        .await;
        assert!(matches!(out, Err(ExtConsentError::Store(_))));
        assert!(ledger.current().grant_events.is_empty());
    }

    // ── the folder read twin (`webdav-server.md` § Key model → *A principal's read*) ──

    const FOLDER_READ: &str = "fauna:folder:read:42";

    /// A set the owner's custody holds two generations for, at its serve
    /// pseudo-channel, served or not as `served` says.
    fn folder(name: &str, served: bool) -> ConsentFolder {
        use fauna_core::data::FolderKeyCustody;
        use fauna_core::folder_keys::{FolderContentKeys, serve_custody_channel_id};
        let channel = serve_custody_channel_id(name);
        let mut keys = FolderContentKeys::genesis([0xC1; 32], 1_000);
        keys.rotate([0xC2; 32], 2_000);
        let mut custody = fauna_core::data::FoldersConfig::default();
        custody.sets.push(FolderKeyCustody {
            channel_id: Some(channel),
            keys: Some(keys),
            ..Default::default()
        });
        ConsentFolder {
            set_name: name.into(),
            served_by_custody: served,
            custody_channel_id: Some(channel),
            custody,
        }
    }

    struct Owner {
        kp: ActorKeypair,
        delegable: DelegableSchedule,
        manifests: FakeKindManifestStore,
        ledger: FakeSuccessionLedgerStore,
        signer: KeypairSigner,
    }

    impl Owner {
        fn new() -> Self {
            let kp = ActorKeypair::generate();
            let ledger = FakeSuccessionLedgerStore::empty(kp.actor_id());
            let signer = KeypairSigner(ActorKeypair::from_secret(*kp.secret_bytes()));
            Self {
                kp,
                delegable: DelegableSchedule::derive(&BackupKey::derive(&[7u8; 32])),
                manifests: FakeKindManifestStore::empty(),
                ledger,
                signer,
            }
        }

        async fn prepare(
            &self,
            c: &PendingConsentRow,
            folders: &BTreeMap<i64, ConsentFolder>,
        ) -> Result<Vec<PreparedExtGrant>, ExtConsentError> {
            prepare_ext_consent_grant(
                &ExtConsentOwner {
                    actor_id: self.kp.actor_id().0,
                    owner_secret: self.kp.secret_bytes(),
                    delegable: &self.delegable,
                    manifests: &self.manifests,
                    ledger: &self.ledger,
                    signer: &self.signer,
                },
                c,
                folders,
                [9; 16],
                1_000,
            )
            .await
        }

        /// The derived id of `holder`'s grant over `set` at `generation`.
        fn twin_id(&self, holder: &[u8; 32], set: &str, generation: u32) -> [u8; 16] {
            folder_principal_grant_id(self.kp.secret_bytes(), holder, set, generation)
        }

        /// The ids the log's `Mint` events name, sorted (the merged log keeps
        /// its own order).
        fn minted(&self) -> Vec<Vec<u8>> {
            let mut ids: Vec<Vec<u8>> = self
                .ledger
                .current()
                .grant_events
                .iter()
                .filter(|e| e.kind == fauna_core::grant_event::GrantEventKind::Mint)
                .map(|e| e.grant_id.clone())
                .collect();
            ids.sort();
            ids
        }
    }

    /// A served folder plans its own grant — one `content.read{folder, set}`
    /// tuple carrying one wrap per content-key generation, under the derived
    /// generation-0 id and one recorded `Mint` — and a folder-only consent
    /// mints no records grant, needs no manifest and publishes no row.
    #[tokio::test]
    async fn a_served_folder_plans_one_wrap_per_generation() {
        let owner = Owner::new();
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let mut c = consent(&[FOLDER_READ], holder_pk, None);
        c.fauna_manifest = None;
        let folders = BTreeMap::from([(42, folder("photos", true))]);

        let prepared = owner.prepare(&c, &folders).await.unwrap();

        assert_eq!(prepared.len(), 1, "a folder-only consent: no records grant");
        let prepared = &prepared[0];
        assert_eq!(prepared.grant_id, owner.twin_id(&holder_pk, "photos", 0));
        assert!(!prepared.replaced);
        assert_eq!(prepared.folders, [42]);
        assert!(prepared.kinds.is_empty());
        assert!(owner.manifests.row(CLIENT).is_none(), "no manifest row");
        assert_eq!(owner.minted(), [prepared.grant_id.to_vec()]);
        let blob = GrantBlob::from_canonical_bytes(&prepared.blob_bytes).unwrap();
        let hash = fauna_core::path_crypto::set_name_hash("photos");
        assert_eq!(blob.scope, [ScopeTuple::folder_read(&hash)]);
        let mut opened: Vec<(u64, Vec<u8>)> = blob
            .wrapped_keys
            .iter()
            .map(|w| {
                let owner_id = owner.kp.actor_id().0;
                (
                    w.epoch.expect("a folder wrap names its generation"),
                    fauna_mls::wrapped_blob::unseal_capability(w, &owner_id, &holder_sk).unwrap(),
                )
            })
            .collect();
        opened.sort_by_key(|(v, _)| *v);
        assert_eq!(opened, [(1, vec![0xC1; 32]), (2, vec![0xC2; 32])]);
    }

    /// The principal's side of that twin: its holder key opens the grant into
    /// the set's whole generation list — the bundle the `Fauna-Folder-Keys`
    /// header carries, which the MDA's decode reads back unchanged — and a
    /// foreign key, or a blob that is no folder twin, opens nothing.
    #[tokio::test]
    async fn the_principal_opens_its_twin_into_the_keys_header_bundle() {
        use fauna_core::folder_keys::FolderContentKeys;
        let owner = Owner::new();
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let mut c = consent(&[FOLDER_READ], holder_pk, None);
        c.fauna_manifest = None;
        let folders = BTreeMap::from([(42, folder("photos", true))]);
        let prepared = owner.prepare(&c, &folders).await.unwrap();
        let blob = GrantBlob::from_canonical_bytes(&prepared[0].blob_bytes).unwrap();

        let keys = crate::open_folder_read_keys(&blob, &holder_sk).unwrap();
        let mut versions: Vec<(u64, [u8; 32])> = keys
            .generations()
            .map(|g| (g.version, g.key.to_array()))
            .collect();
        versions.sort_by_key(|(v, _)| *v);
        assert_eq!(versions, [(1, [0xC1; 32]), (2, [0xC2; 32])]);
        assert_eq!(keys.current.version, 2, "the newest generation is current");
        let header = keys.to_header_value().unwrap();
        assert_eq!(FolderContentKeys::from_header_value(&header).unwrap(), keys);

        let (stranger, _) = generate_x25519_keypair();
        assert!(crate::open_folder_read_keys(&blob, &stranger).is_err());
        let no_folder = GrantBlob {
            scope: Vec::new(),
            wrapped_keys: Vec::new(),
            ..blob
        };
        assert!(crate::open_folder_read_keys(&no_folder, &holder_sk).is_err());
    }

    /// An unserved folder is refused with the card's reason, and nothing is
    /// recorded — the approve never flips the serve flag.
    #[tokio::test]
    async fn an_unserved_folder_is_refused() {
        let owner = Owner::new();
        let c = consent(&[FOLDER_READ], [1; 32], None);
        let folders = BTreeMap::from([(42, folder("photos", false))]);
        let err = owner.prepare(&c, &folders).await.unwrap_err();
        assert!(matches!(&err, ExtConsentError::FolderNotServed(n) if n == "photos"));
        assert!(err.to_string().contains("not served over WebDAV"));
        assert!(owner.ledger.current().grant_events.is_empty());
    }

    /// A served folder this device holds no keys for refuses (fail closed),
    /// as does a folder the custody seam did not resolve at all.
    #[tokio::test]
    async fn a_keyless_or_unresolved_folder_is_refused() {
        let owner = Owner::new();
        let c = consent(&[FOLDER_READ], [1; 32], None);

        let mut keyless = folder("photos", true);
        keyless.custody = fauna_core::data::FoldersConfig::default();
        let err = owner
            .prepare(&c, &BTreeMap::from([(42, keyless)]))
            .await
            .unwrap_err();
        assert!(matches!(err, ExtConsentError::FolderNoKeys(n) if n == "photos"));

        let mut unchanneled = folder("photos", true);
        unchanneled.custody_channel_id = None;
        let err = owner
            .prepare(&c, &BTreeMap::from([(42, unchanneled)]))
            .await
            .unwrap_err();
        assert!(matches!(err, ExtConsentError::FolderNoKeys(_)));

        let err = owner.prepare(&c, &BTreeMap::new()).await.unwrap_err();
        assert!(matches!(err, ExtConsentError::FolderUnknown(42)));
        assert!(owner.ledger.current().grant_events.is_empty());
    }

    /// A folder read without an attested holder key cannot be granted.
    #[tokio::test]
    async fn a_folder_read_without_a_holder_key_is_refused() {
        let owner = Owner::new();
        let mut c = consent(&[FOLDER_READ], [1; 32], None);
        c.holder_x25519 = None;
        let folders = BTreeMap::from([(42, folder("photos", true))]);
        let err = owner.prepare(&c, &folders).await.unwrap_err();
        assert!(matches!(err, ExtConsentError::NoHolderKey));
    }

    /// Records and a folder read mint TWO grants under two `Mint`s: the
    /// records grant first under the caller's id, then the folder twin as its
    /// own grant under the derived id over (holder, "photos") — both wrapped
    /// to the one holder.
    #[tokio::test]
    async fn records_and_a_folder_read_mint_two_grants() {
        let owner = Owner::new();
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let c = consent(
            &["fauna:records:rw:ext.app.example.*", FOLDER_READ],
            holder_pk,
            Some([0x5A; 32]),
        );
        let folders = BTreeMap::from([(42, folder("photos", true))]);

        let prepared = owner.prepare(&c, &folders).await.unwrap();

        assert_eq!(prepared.len(), 2);
        let (records, twin) = (&prepared[0], &prepared[1]);
        assert_eq!(records.grant_id, [9; 16]);
        assert_eq!(records.kinds.len(), 2);
        assert!(records.folders.is_empty());
        assert_eq!(twin.grant_id, owner.twin_id(&holder_pk, "photos", 0));
        assert_eq!(twin.folders, [42]);
        assert!(owner.manifests.row(CLIENT).is_some());
        let mut both = vec![records.grant_id.to_vec(), twin.grant_id.to_vec()];
        both.sort();
        assert_eq!(owner.minted(), both, "two grants, two Mint events");
        let records_blob = GrantBlob::from_canonical_bytes(&records.blob_bytes).unwrap();
        // 2 kinds × (read + write), and no folder tuple.
        assert_eq!(records_blob.scope.len(), 4);
        assert_eq!(
            crate::open_ext_kind_keys(&records_blob, &holder_sk)
                .unwrap()
                .len(),
            2
        );
        let twin_blob = GrantBlob::from_canonical_bytes(&twin.blob_bytes).unwrap();
        let hash = fauna_core::path_crypto::set_name_hash("photos");
        assert_eq!(twin_blob.scope, [ScopeTuple::folder_read(&hash)]);
    }

    /// An oracle scope alone mints ONE keyless grant under the caller's id —
    /// one `identity.op` tuple per named class, no wrap, one `Mint` — and
    /// publishes no manifest row; a sovereign name is no oracle scope at all.
    #[tokio::test]
    async fn an_oracle_scope_mints_one_keyless_identity_op_grant() {
        use fauna_core::identity_op::IdentityOpClass;
        let owner = Owner::new();
        let (_, holder_pk) = generate_x25519_keypair();
        let c = consent(
            &[
                "fauna:identity:op:post.author",
                "fauna:identity:op:nostr.sign_event",
                "fauna:identity:op:atproto.plc_rotate",
                "fauna:feed:read",
            ],
            holder_pk,
            None,
        );

        let prepared = owner.prepare(&c, &BTreeMap::new()).await.unwrap();

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].grant_id, [9; 16]);
        assert!(owner.manifests.row(CLIENT).is_none());
        assert_eq!(owner.minted(), [vec![9u8; 16]]);
        let blob = GrantBlob::from_canonical_bytes(&prepared[0].blob_bytes).unwrap();
        assert_eq!(
            blob.scope,
            [
                crate::identity_op_scope(IdentityOpClass::NostrSignEvent),
                crate::identity_op_scope(IdentityOpClass::PostAuthor),
            ]
        );
        assert!(
            blob.wrapped_keys.is_empty(),
            "the tuple is keyless: the key stays with its custodian"
        );
    }

    /// Records and an oracle scope share the consent's ONE primary grant: the
    /// records tuples and the `identity.op` tuple, under the caller's id.
    #[tokio::test]
    async fn records_and_an_oracle_scope_share_one_grant() {
        use fauna_core::identity_op::IdentityOpClass;
        let owner = Owner::new();
        let (_, holder_pk) = generate_x25519_keypair();
        let c = consent(
            &[
                "fauna:records:rw:ext.app.example.*",
                "fauna:identity:op:post.author",
            ],
            holder_pk,
            Some([0x5A; 32]),
        );

        let prepared = owner.prepare(&c, &BTreeMap::new()).await.unwrap();

        assert_eq!(prepared.len(), 1);
        let blob = GrantBlob::from_canonical_bytes(&prepared[0].blob_bytes).unwrap();
        // 2 kinds × (read + write), and the oracle tuple.
        assert_eq!(blob.scope.len(), 5);
        assert!(
            blob.scope
                .contains(&crate::identity_op_scope(IdentityOpClass::PostAuthor))
        );
    }

    /// Records and two folders mint three grants, the twins in folder-id
    /// order, each under its own set's derived id.
    #[tokio::test]
    async fn two_folders_mint_three_grants() {
        let owner = Owner::new();
        let (_, holder_pk) = generate_x25519_keypair();
        let c = consent(
            &[
                "fauna:records:rw:ext.app.example.*",
                FOLDER_READ,
                "fauna:folder:read:7",
            ],
            holder_pk,
            None,
        );
        let folders = BTreeMap::from([(42, folder("photos", true)), (7, folder("docs", true))]);

        let prepared = owner.prepare(&c, &folders).await.unwrap();

        let ids: Vec<[u8; 16]> = prepared.iter().map(|p| p.grant_id).collect();
        assert_eq!(
            ids,
            [
                [9; 16],
                owner.twin_id(&holder_pk, "docs", 0),
                owner.twin_id(&holder_pk, "photos", 0),
            ]
        );
        assert_eq!(prepared[1].folders, [7]);
        assert_eq!(prepared[2].folders, [42]);
        assert_eq!(owner.minted().len(), 3);
    }

    /// A second approve of the same (holder, set) derives the same id and
    /// re-mints in place (`replaced`); after a revoke the next approve mints
    /// generation 1 — a different, fresh id.
    #[tokio::test]
    async fn a_reconsent_replaces_in_place_and_a_revoke_moves_the_generation() {
        let owner = Owner::new();
        let (_, holder_pk) = generate_x25519_keypair();
        let mut c = consent(&[FOLDER_READ], holder_pk, None);
        c.fauna_manifest = None;
        let folders = BTreeMap::from([(42, folder("photos", true))]);

        let first = owner.prepare(&c, &folders).await.unwrap();
        let again = owner.prepare(&c, &folders).await.unwrap();
        assert_eq!(again[0].grant_id, first[0].grant_id);
        assert!(!first[0].replaced);
        assert!(
            again[0].replaced,
            "the live generation is re-minted in place"
        );

        grant_log::record_revokes(
            &owner.ledger,
            &owner.signer,
            owner.kp.actor_id().0,
            &[(first[0].grant_id, holder_pk)],
            2_000,
        )
        .await
        .unwrap();
        let after = owner.prepare(&c, &folders).await.unwrap();
        assert_eq!(after[0].grant_id, owner.twin_id(&holder_pk, "photos", 1));
        assert_ne!(after[0].grant_id, first[0].grant_id);
        assert!(!after[0].replaced, "a spent generation is never re-minted");
    }

    /// The twin-bearing predicate and the id walk the caller resolves by.
    #[test]
    fn twin_scopes_and_folder_ids() {
        assert!(names_twin_scope("fauna:records:rw:ext.app.example.*"));
        assert!(names_twin_scope(FOLDER_READ));
        assert!(names_twin_scope("fauna:folder:deposit:42"));
        assert!(
            !names_twin_scope("fauna:folder:deposit"),
            "a bare arm names no folder until the card's choice qualifies it"
        );
        assert!(!names_twin_scope("fauna:feed:read"));
        let scopes: Vec<String> = ["fauna:folder:read:7", FOLDER_READ, "fauna:folder:read:7"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(folder_read_ids(&scopes), [7, 42]);
    }

    // ── The fifth arm's twin (`authorization-server.md` § Scope grammar →
    // *The fifth arm*) ──

    const PERSONALIZATION: &str = "fauna:personalization:rw";

    /// The pair a holder unwraps from `blob`'s tuple over `kind`.
    fn unwrapped(blob: &GrantBlob, owner: &[u8; 32], holder_sk: &[u8; 32], kind: &str) -> Vec<u8> {
        let w = blob
            .wrapped_keys
            .iter()
            .find(|w| w.scope.kind.as_deref() == Some(kind))
            .expect("the tuple is wrapped");
        fauna_mls::wrapped_blob::unseal_capability(w, owner, holder_sk).unwrap()
    }

    /// The arm alone mints the consent's one grant under the caller's id: two
    /// `content.read` tuples — the registry kind and the model kind — each
    /// wrapping exactly the owner's delegable pair for that kind, one `Mint`,
    /// no manifest needed and no row published.
    #[tokio::test]
    async fn the_personalization_arm_mints_the_registry_and_model_pairs() {
        let owner = Owner::new();
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let mut c = consent(&[PERSONALIZATION, "openid"], holder_pk, None);
        c.fauna_manifest = None;
        assert!(c.scopes.iter().any(|s| names_twin_scope(s)));

        let prepared = owner.prepare(&c, &BTreeMap::new()).await.unwrap();

        assert_eq!(prepared.len(), 1);
        let prepared = &prepared[0];
        assert_eq!(prepared.grant_id, [9; 16]);
        assert!(prepared.kinds.is_empty() && prepared.folders.is_empty());
        assert!(owner.manifests.row(CLIENT).is_none(), "no manifest row");
        assert_eq!(owner.minted(), [prepared.grant_id.to_vec()]);
        let blob = GrantBlob::from_canonical_bytes(&prepared.blob_bytes).unwrap();
        assert_eq!(
            blob.scope,
            [
                ScopeTuple::delegable_kind_read("fauna.state.personalization"),
                ScopeTuple::delegable_kind_read("fauna.personalization.model"),
            ]
        );
        let owner_id = owner.kp.actor_id().0;
        for kind in crate::PERSONALIZATION_GRANT_KINDS {
            let (entry_key, item_blind) = owner.delegable.for_kind(kind).to_grant();
            assert_eq!(
                unwrapped(&blob, &owner_id, &holder_sk, kind),
                [entry_key, item_blind].concat(),
                "{kind}: the ordinary delegable unit"
            );
        }
        let opened = crate::open_personalization_keys(&blob, &holder_sk).unwrap();
        let mut kinds: Vec<&str> = opened.iter().map(|k| k.kind()).collect();
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            ["fauna.personalization.model", "fauna.state.personalization"]
        );
        assert!(
            crate::open_ext_kind_keys(&blob, &holder_sk)
                .unwrap()
                .is_empty(),
            "the twin opens no ext kind"
        );
    }

    /// Nothing to wrap to: a ceremony with no attested holder key refuses the
    /// arm with the records arm's own refusal, and records nothing.
    #[tokio::test]
    async fn the_personalization_arm_without_a_holder_key_is_refused() {
        let owner = Owner::new();
        let mut c = consent(&[PERSONALIZATION], [1; 32], None);
        c.holder_x25519 = None;
        let err = owner.prepare(&c, &BTreeMap::new()).await.unwrap_err();
        assert!(matches!(err, ExtConsentError::NoHolderKey));
        assert!(owner.ledger.current().grant_events.is_empty());
    }

    /// Beside records, the twin rides the records grant — one blob, one
    /// `Mint` — and each opener opens only its own kinds.
    #[tokio::test]
    async fn records_and_the_personalization_arm_share_the_consents_grant() {
        let owner = Owner::new();
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let c = consent(
            &["fauna:records:rw:ext.app.example.*", PERSONALIZATION],
            holder_pk,
            Some([0x5A; 32]),
        );

        let prepared = owner.prepare(&c, &BTreeMap::new()).await.unwrap();

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].kinds.len(), 2);
        assert_eq!(owner.minted(), [vec![9u8; 16]]);
        let blob = GrantBlob::from_canonical_bytes(&prepared[0].blob_bytes).unwrap();
        // 2 kinds × (read + write) + the twin's two reads.
        assert_eq!(blob.scope.len(), 6);
        assert_eq!(
            crate::open_ext_kind_keys(&blob, &holder_sk).unwrap().len(),
            2
        );
        assert_eq!(
            crate::open_personalization_keys(&blob, &holder_sk)
                .unwrap()
                .len(),
            2
        );
    }

    // ── The folder deposit twin (`file-sync.md` § Third-party deposit
    // ingress; `third-party-kinds.md` § *Who mints the consent-time grant*:
    // the keyless tuple rides the consent's one grant) ──

    const DEPOSIT: &str = "fauna:folder:deposit:7";

    /// A consent naming a deposit scope and an attested holder key plans one
    /// keyless `deposit` tuple over the folder into the consent's one grant —
    /// under the caller's id, one recorded `Mint`, no manifest, no wraps.
    #[tokio::test]
    async fn a_deposit_scope_plans_one_keyless_tuple_into_the_consents_grant() {
        let owner = Owner::new();
        let (_, holder_pk) = generate_x25519_keypair();
        let mut c = consent(&[DEPOSIT, DEPOSIT], holder_pk, None);
        c.fauna_manifest = None;

        let prepared = owner.prepare(&c, &BTreeMap::new()).await.unwrap();

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].grant_id, [9; 16]);
        assert!(prepared[0].kinds.is_empty() && prepared[0].folders.is_empty());
        assert!(!prepared[0].replaced);
        assert_eq!(owner.minted(), [vec![9u8; 16]]);
        assert!(owner.manifests.row(CLIENT).is_none(), "no manifest row");
        let blob = GrantBlob::from_canonical_bytes(&prepared[0].blob_bytes).unwrap();
        assert_eq!(blob.scope, [ScopeTuple::folder_deposit(7)]);
        assert!(blob.wrapped_keys.is_empty(), "deposit is keyless");
    }

    /// Beside records, the deposit tuples ride the records grant — one blob,
    /// one `Mint`.
    #[tokio::test]
    async fn records_and_a_deposit_scope_share_the_consents_grant() {
        let owner = Owner::new();
        let (_, holder_pk) = generate_x25519_keypair();
        let c = consent(
            &[
                "fauna:records:rw:ext.app.example.*",
                "fauna:folder:deposit:9",
                DEPOSIT,
            ],
            holder_pk,
            None,
        );

        let prepared = owner.prepare(&c, &BTreeMap::new()).await.unwrap();

        assert_eq!(prepared.len(), 1);
        assert_eq!(owner.minted(), [vec![9u8; 16]]);
        let blob = GrantBlob::from_canonical_bytes(&prepared[0].blob_bytes).unwrap();
        let deposits: Vec<&ScopeTuple> = blob
            .scope
            .iter()
            .filter(|t| t.class == ScopeTuple::CLASS_DEPOSIT)
            .collect();
        assert_eq!(
            deposits,
            [
                &ScopeTuple::folder_deposit(7),
                &ScopeTuple::folder_deposit(9)
            ],
            "one tuple per folder, in folder-id order"
        );
        assert_eq!(blob.scope.len(), 4, "2 kinds' reads + 2 deposits");
    }

    /// No attested holder key → the consent cannot be granted, the records
    /// arm's own answer; nothing is recorded.
    #[tokio::test]
    async fn a_deposit_scope_without_a_holder_key_is_refused() {
        let owner = Owner::new();
        let mut c = consent(&[DEPOSIT], [1; 32], None);
        c.holder_x25519 = None;
        let err = owner.prepare(&c, &BTreeMap::new()).await.unwrap_err();
        assert!(matches!(err, ExtConsentError::NoHolderKey));
        assert!(owner.ledger.current().grant_events.is_empty());
    }

    /// A bare folder-plane scope reaching the planner unqualified is refused
    /// before anything is written — never resolved keyless.
    #[tokio::test]
    async fn a_bare_folder_scope_reaching_the_planner_is_refused() {
        let owner = Owner::new();
        for bare in ["fauna:folder:deposit", "fauna:folder:read"] {
            let c = consent(&[bare], [1; 32], None);
            let err = owner.prepare(&c, &BTreeMap::new()).await.unwrap_err();
            assert!(
                matches!(err, ExtConsentError::FolderUnchosen),
                "{bare}: {err}"
            );
        }
        assert!(owner.ledger.current().grant_events.is_empty());
    }

    /// The card's choice qualifies every bare folder-plane scope; a bare row
    /// with no choice, a choice on a row that takes none, and a choice that
    /// is no folder id are each refused.
    #[test]
    fn the_cards_choice_qualifies_the_bare_scopes() {
        let scopes = |s: &[&str]| s.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            approved_scopes(&scopes(&["fauna:folder:deposit", "openid"]), Some(7)).unwrap(),
            scopes(&[DEPOSIT, "openid"])
        );
        assert_eq!(
            approved_scopes(&scopes(&[DEPOSIT]), None).unwrap(),
            scopes(&[DEPOSIT]),
            "a qualified row takes no choice"
        );
        assert!(matches!(
            approved_scopes(&scopes(&["fauna:folder:deposit"]), None),
            Err(ExtConsentError::FolderUnchosen)
        ));
        assert!(matches!(
            approved_scopes(&scopes(&[DEPOSIT]), Some(7)),
            Err(ExtConsentError::FolderChoiceNotTaken)
        ));
        assert!(matches!(
            approved_scopes(&scopes(&["fauna:folder:deposit"]), Some(0)),
            Err(ExtConsentError::FolderUnknown(0))
        ));
        assert_eq!(
            folder_deposit_ids(&scopes(&["fauna:folder:deposit:9", DEPOSIT, DEPOSIT])),
            [7, 9]
        );
    }
}
