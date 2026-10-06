//! Client-side capability mint — the § 2.6 "derive payloads" half: given the
//! owner's custodies (the mail custody, the period-key custody, the folder-key
//! custody — account-plane rows the caller reads) and a declared scope, derive
//! each key-bearing tuple's minimal payload and assemble a `GrantBlob` via
//! `fauna_mls::wrapped_blob::build_grant_blob` (already landed).
//!
//! Design authority: the capability-mediated content-processing design
//! (tracked internally), § 2.1 (the scope-taxonomy -> payload table) and
//! § 2.6 ("Left to the Phase-2 build: … the client-side mint (derive
//! payloads + build `GrantBlob`)"). Touches only material the owner's client already holds
//! (MSEK, a tier's `period_key`) — never the identity seed, MLS group state,
//! or the content-index master key (`key-material-hierarchy.md` rule #7), so
//! it stays wasm-clean and identity-free.

// The boundary rows' UniFFI scaffolding — compiled only when a native FFI
// consumer (fauna-ffi) asks for it. The web SPA's wasm build leaves it off and
// reads the very same `custody_view` rows through serde, which is what lets ONE
// projection serve both faces. Mirrors `fauna-client-pair`.
#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_client_capabilities");

pub mod custody_acts;
pub mod custody_ceremony;
pub mod custody_grants;
pub mod custody_hosting;
pub mod custody_view;
pub mod ext_consent;
pub mod grant_log;
pub mod group_ceremony;
#[cfg(feature = "p2p-share")]
pub mod group_ceremony_node;
#[cfg(feature = "p2p-share")]
pub mod group_ceremony_peer;
#[cfg(feature = "p2p-share")]
pub mod group_ceremony_view;
pub mod rpc;
pub mod succession;
pub mod trust_clock;
pub mod view_model;

pub use succession::{
    GrantRemintError, GrantRemintOutcome, GrantRemintProgress, remint_capability_grants,
};

use fauna_client_subscriptions::custody::current_period;
use fauna_core::data::{MailConfig, SubscriptionsConfig};
use fauna_mls::wrapped_blob::{
    GrantBlob, GrantWindow, PriorMsekGeneration, ScopeTuple, ScopeWraps, WrapError,
    WrappedScopeKey, bounded_mail_epoch_wraps, bounded_mail_epoch_wraps_for_range,
    build_bounded_mail_grant, build_grant_blob, build_grant_blob_with_epochs, build_renewal_wraps,
    derive_recipient_mail_capability_secret, mail_sealing_epoch_of,
};

/// Advisory default grant window: ~90 days, in seconds — `GrantWindow`'s wire
/// unit (mirrors `bins/fauna-nest/src/db/mod.rs::now_epoch_secs`). Every
/// content kind today is standing-keyed, so this bounds only
/// *re-acquisition*, not content already reachable via the standing key
/// (design § Phase 2 Step 1 (e), the honest "standing-keyed" settings-page
/// bound). Callers add this to their own `now_epoch_secs` to get
/// `window.1` — the crate has no clock of its own (pure + wasm-clean).
pub const DEFAULT_GRANT_WINDOW_SECS: u64 = 90 * 24 * 60 * 60;

/// The **one-off** grant window: 8 hours, in seconds — the short window a
/// grant gets when the user picks *a few hours* on the mint flow, and the
/// default on a nest they have not blessed (`docs/goal/ui/nests.md` § Expiry /
/// renewal → *Duration and blessing*). Shorter than
/// [`view_model::RENEW_AHEAD_SECS`], so the auto-renew loop never renews it: a
/// one-off grant lasts its hours and lapses. Hard-coded, no config surface.
pub const ONE_OFF_GRANT_WINDOW_SECS: u64 = 8 * 60 * 60;

/// The per-grant duration the user picks on `nest-trust-mint-duration-select`
/// — one of the two hard-coded windows above (`nests.md` § Expiry / renewal →
/// *Duration and blessing*: "the only user choices are per-grant duration +
/// blessed-box status").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantDuration {
    /// [`ONE_OFF_GRANT_WINDOW_SECS`] — a few hours, never auto-renewed.
    OneOff,
    /// [`DEFAULT_GRANT_WINDOW_SECS`] — ~90 days, auto-renewed on a blessed nest.
    Standard,
}

impl GrantDuration {
    /// The window length this duration mints, in seconds.
    pub const fn secs(self) -> u64 {
        match self {
            Self::OneOff => ONE_OFF_GRANT_WINDOW_SECS,
            Self::Standard => DEFAULT_GRANT_WINDOW_SECS,
        }
    }

    /// The duration a mint takes when the user names none, and the one the
    /// picker pre-selects: standard on a blessed nest (its grants renew
    /// themselves), one-off on an un-blessed one.
    pub const fn default_for(blessed: bool) -> Self {
        if blessed {
            Self::Standard
        } else {
            Self::OneOff
        }
    }
}

/// A declared scope tuple's minimal payload could not be derived from the
/// owner's current custody.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DerivePayloadError {
    /// `content.read{mail|calendar}` needs the mail custody's `msek`, set only
    /// after the user's first "Enable mail" flow (`mail-credentials.md`).
    #[error(
        "mail/calendar capability requires mail to be enabled (the mail custody holds no MSEK)"
    )]
    MailNotEnabled,
    /// `content.read{post}` needs a `tier` component naming which tier.
    #[error("content.read{{post}} scope tuple is missing its tier")]
    PostMissingTier,
    /// `content.read{post:tier}` needs a period key, held only if the author
    /// created that tier in encrypted mode (`custody::current_period`).
    #[error("no period key held for tier {0:?} (not created in encrypted mode)")]
    TierNotFound(String),
    /// `content.read{post:tier}` was asked of a caller that read no period-key
    /// custody (the store was not up, or the caller mints no post tier) —
    /// never answered as "no key held", which is [`Self::TierNotFound`].
    #[error("the period-key custody was not read, so tier {0:?}'s key cannot be derived")]
    PeriodKeysUnread(String),
    /// `index.write{kind}` / `index.read{kind}` need a per-kind
    /// index-segment key. **Not yet buildable**: `content-index.md` §
    /// Encryption posture marks the MSEK-derived per-kind key + split
    /// manifest as target state, "Plan 5b, not started" — no derivation
    /// exists in `libs/fauna-index` today. Resolve there before this arm.
    #[error(
        "index.{{write,read}} capabilities are blocked on Plan 5b (encrypted-mode index ingest, not started — see docs/goal/behavior/content-index.md § Encryption posture)"
    )]
    IndexNotImplemented,
    /// `content.read{folder:set}` payloads are per-generation and need the
    /// set's custody identity resolved by the caller (the orchestration layer
    /// holds the set summary) — use [`mint_folder_grant`], not the generic
    /// [`derive_scope_payload`] path.
    #[error(
        "content.read{{folder}} grants are minted via mint_folder_grant (per-generation wraps need the set's resolved custody channel)"
    )]
    FolderUsesDedicatedMint,
    /// [`mint_folder_grant`] found no content-key custody entry at the
    /// resolved channel id — the set has no content keys yet (paywalling
    /// drives genesis first), or the caller resolved the wrong identity
    /// (pseudo vs real channel — `webdav-server.md` § Key model custody note).
    #[error("no folder content-key custody at the resolved channel for set {0:?}")]
    FolderCustodyNotFound(String),
    /// A `content.read` tuple names a `kind` outside
    /// `{mail, calendar, post, folder, spam-model}` (§ taxonomy).
    #[error("content.read scope tuple has an unrecognized or missing kind: {0:?}")]
    UnknownReadKind(Option<String>),
    /// A tuple's `class` is outside the four taxonomy classes.
    #[error("unrecognized scope class: {0:?}")]
    UnknownClass(String),
    /// An `identity.op` tuple's `kind` names no built operation class — or
    /// names a sovereign operation, which is never delegable
    /// (`fauna_core::identity_op`, the deny list).
    #[error("identity.op scope tuple refused: {0}")]
    IdentityOpRefused(String),
    /// A `content.read` tuple over an `ext.*` kind wraps that kind's delegable
    /// pair, derived from the owner's account-state schedule — use
    /// [`mint_ext_kinds_grant`], not the generic [`derive_scope_payload`]
    /// path (the folder grant's dedicated-mint shape).
    #[error("content.read over an ext.* kind is minted via mint_ext_kinds_grant")]
    ExtKindUsesDedicatedMint,
    /// A `content.write` tuple must name one `ext.*` kind and carry exactly
    /// one canonical `writer:<hex>` factor (`third-party-kinds.md`
    /// § Principal write authority).
    #[error("content.write scope tuple refused: {0}")]
    ContentWriteRefused(String),
    /// A `deposit` tuple must name one folder by its row id in decimal and
    /// carry nothing else (`file-sync.md` § Third-party deposit ingress).
    #[error("deposit scope tuple refused: {0}")]
    DepositRefused(String),
}

/// Derive the minimal payload a [`ScopeTuple`] wraps into its
/// `WrappedScopeKey`, from material the owner already holds — the mail
/// custody (`fauna.state.mail`, the account plane's READ fold) for the
/// MSEK-derived kinds, the period-key custody (`fauna.state.subscriptions`,
/// read through `fauna_client_subscriptions::PeriodKeyStore`) for a post
/// tier — `None` when the caller read no custody; the design's § 2.1 table
/// made concrete. Returns `Ok(None)` for the two
/// keyless shapes — the `content.label-write` class and the
/// `content.read{spam-model}` kind (the artifact travels as a
/// sealed-to-holder copy instead of a key — `key-material-hierarchy.md`
/// rule #7 resolution, ratified 2026-07-13); every other class either derives
/// a payload or reports why it can't (mail not enabled, tier not held, or —
/// for the two `index.*` classes — a genuine not-yet-built dependency).
pub fn derive_scope_payload(
    period_keys: Option<&SubscriptionsConfig>,
    mail: &MailConfig,
    tuple: &ScopeTuple,
) -> Result<Option<Vec<u8>>, DerivePayloadError> {
    match tuple.class.as_str() {
        ScopeTuple::CLASS_CONTENT_READ => match tuple.kind.as_deref() {
            Some(ScopeTuple::KIND_MAIL) | Some(ScopeTuple::KIND_CALENDAR) => {
                let msek = mail
                    .msek
                    .as_ref()
                    .ok_or(DerivePayloadError::MailNotEnabled)?;
                // The `32 + 2400`-byte X-Wing shape (`x25519_secret ∥ ml-kem-dk`) —
                // the goal-doc payload contract (`post-quantum.md` § surface A, the
                // derived-High capability-grant note). Assembled by the shared
                // `fauna_mls::wrapped_blob::derive_recipient_mail_capability_secret`
                // (the single source of truth for the byte contract — the `fauna-ffi`
                // recipient-material export the tier_3 seal-helper mint drives derives
                // it from the SAME function, so the shape can't drift). The X25519 half
                // is byte-identical to the classical recipient secret, so this is a pure
                // superset: the holder's `open_mail_record_with_key` (32-vs-2432 length
                // dispatch) opens BOTH classical and hybrid (X-Wing-sealed) mail records
                // with it, which is what makes hybrid-sealed mail drainable under the
                // grant. Deterministic in MSEK, so no "PQ published" gate is
                // needed or available — and no larger than the derived key already is
                // off-box.
                Ok(Some(derive_recipient_mail_capability_secret(msek)))
            }
            Some(ScopeTuple::KIND_POST) => {
                let tier = tuple
                    .tier
                    .as_deref()
                    .ok_or(DerivePayloadError::PostMissingTier)?;
                let custody = period_keys
                    .ok_or_else(|| DerivePayloadError::PeriodKeysUnread(tier.to_string()))?;
                let period = current_period(custody, tier)
                    .ok_or_else(|| DerivePayloadError::TierNotFound(tier.to_string()))?;
                Ok(Some(period.key.to_vec()))
            }
            Some(ScopeTuple::KIND_FOLDER) => Err(DerivePayloadError::FolderUsesDedicatedMint),
            // KEYLESS by ratified design — never derive a payload here. The
            // spam model's only opening key is the recipient-mail secret (the
            // `content.read{mail}` payload above), so any key this arm could
            // wrap would convey the whole mailbox; the grant is the
            // audit/revocation record + publish-worklist authorization, and
            // the contributed model travels as a `SpamModelCopyBlob` sealed
            // to the holder's own pubkey (`seal_spam_model_copy`).
            // `key-material-hierarchy.md` rule #7 + § Don't do these;
            // `mail-spam.md` § Encrypted-mode interaction (2026-07-13).
            Some(ScopeTuple::KIND_SPAM_MODEL) => Ok(None),
            Some(kind) if fauna_protocol::ext_kind::is_ext_kind(kind) => {
                Err(DerivePayloadError::ExtKindUsesDedicatedMint)
            }
            other => Err(DerivePayloadError::UnknownReadKind(
                other.map(str::to_string),
            )),
        },
        ScopeTuple::CLASS_CONTENT_LABEL_WRITE => Ok(None),
        // KEYLESS by ratified design (T13 — `account-data-plane.md`
        // § Replica posture → *The custody grant + ceremony*): the custody
        // row is an authorization + audit record the nest's revocation store
        // answers from, never a key conveyance — a custodian holds no read
        // keys, that being the whole posture. The admission witness is the
        // separate owner-signed `CustodyGrant` envelope
        // (`fauna_core::custody_grant`); vocabulary + mint:
        // `crate::custody_grants`.
        ScopeTuple::CLASS_CUSTODY => Ok(None),
        // KEYLESS by ratified design (TP11 — `key-material-hierarchy.md`
        // § Audience: deployment infrastructure → *The oracle*): the tuple
        // is the audit + revocation record a principal's identity-bearing
        // operations are authorized against; the key never leaves its
        // first-party custodian. The kind must name a BUILT operation class
        // — a sovereign name is refused here exactly as the custodian would
        // refuse it, so no such grant is ever minted.
        ScopeTuple::CLASS_IDENTITY_OP => {
            let kind = tuple.kind.as_deref().unwrap_or_default();
            fauna_core::identity_op::IdentityOpClass::parse(kind)
                .map(|_| None)
                .map_err(|refusal| DerivePayloadError::IdentityOpRefused(refusal.to_string()))
        }
        // KEYLESS by ratified design (`third-party-kinds.md` § Principal
        // write authority): the pair is one symmetric unit with no write
        // half, so the tuple is the owner-signed authorization alone — and it
        // must say exactly whose writing it authorizes, over which one kind.
        ScopeTuple::CLASS_CONTENT_WRITE => {
            check_content_write(tuple).map_err(DerivePayloadError::ContentWriteRefused)?;
            Ok(None)
        }
        // KEYLESS by ratified design (`encryption-at-rest.md` § Capability
        // tiering → *Third-party holders*): the nest seals what the holder
        // deposits to the owner's recipient key, so the tuple is the audit +
        // revocation record alone — over exactly one folder.
        ScopeTuple::CLASS_DEPOSIT => {
            check_deposit(tuple).map_err(DerivePayloadError::DepositRefused)?;
            Ok(None)
        }
        ScopeTuple::CLASS_INDEX_WRITE | ScopeTuple::CLASS_INDEX_READ => {
            Err(DerivePayloadError::IndexNotImplemented)
        }
        other => Err(DerivePayloadError::UnknownClass(other.to_string())),
    }
}

/// A `content.write` tuple's shape: one `ext.*` kind, one canonical writer
/// factor, nothing else.
fn check_content_write(tuple: &ScopeTuple) -> Result<(), String> {
    let kind = tuple.kind.as_deref().unwrap_or_default();
    if !fauna_protocol::ext_kind::is_ext_kind(kind) {
        return Err(format!("{kind:?} is not an ext.* kind"));
    }
    if tuple.tier.is_some() || tuple.set.is_some() {
        return Err("a content.write tuple carries no tier or set".into());
    }
    match tuple.factor.as_deref() {
        Some(f) if fauna_core::grant_event::parse_writer_factor(f).is_some() => Ok(()),
        other => Err(format!("factor {other:?} is not a canonical writer:<hex>")),
    }
}

/// A `deposit` tuple's shape: exactly [`ScopeTuple::folder_deposit`] over
/// some folder id — the canonical decimal spelling, nothing else.
fn check_deposit(tuple: &ScopeTuple) -> Result<(), String> {
    let id = tuple
        .set
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| format!("set {:?} is not a folder id", tuple.set))?;
    if tuple.is_folder_deposit_for(id) {
        Ok(())
    } else {
        Err("a deposit tuple carries only its folder id, canonically spelled".into())
    }
}

/// Mint the **consent-time grant to a third-party principal** over folder
/// deposit (`file-sync.md` § Third-party deposit ingress): one keyless
/// `deposit` tuple per folder, wrapping nothing — the audit + revocation
/// record the nest's deposit door re-resolves at every deposit, held by the
/// principal's attested X25519 key.
///
/// # Errors
///
/// [`MintGrantError::Wrap`] on a malformed holder key.
pub fn mint_folder_deposit_grant(
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    window: GrantWindow,
    folder_ids: &[i64],
) -> Result<GrantBlob, MintGrantError> {
    let scopes: Vec<(ScopeTuple, Option<Vec<u8>>)> = folder_ids
        .iter()
        .map(|id| (ScopeTuple::folder_deposit(*id), None))
        .collect();
    Ok(build_grant_blob(
        owner_actor_id,
        grant_id,
        holder_pubkey,
        None,
        window,
        &scopes,
    )?)
}

/// The delegable pair a `content.read` tuple over an `ext.*` kind wraps:
/// `entry_key ∥ item_blind`, exactly [`fauna_core::crypto::DelegableKindKeys::to_grant`].
pub const EXT_KIND_PAYLOAD_LEN: usize = 64;

/// Mint the **consent-time grant to a third-party principal** over `ext.*`
/// kinds (`third-party-kinds.md` § Principal write authority + § The record
/// doors, step 3): per kind, a `content.read` tuple whose wrap carries the
/// kind's delegable pair, and — when the principal attested a writer key — a
/// keyless `content.write` tuple confined to it. A wildcard scope is expanded
/// to the manifest's declared kinds by the caller, so every tuple names one
/// kind and a prefix never has to be expanded at read time.
///
/// The pair is derived from the owner's **delegable** branch alone
/// (`DelegableSchedule`), which is the only typed path a grant pair leaves
/// (`fauna_core::crypto::DelegableKindKeys::to_grant`); a fleet-only key is
/// unreachable from here by construction.
///
/// # Errors
///
/// [`MintGrantError::Wrap`] on a seal failure (a malformed holder key).
#[allow(clippy::too_many_arguments)] // mint_grant's flat shape + the schedule, kinds and writer
pub fn mint_ext_kinds_grant(
    delegable: &fauna_core::crypto::DelegableSchedule,
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    window: GrantWindow,
    kinds: &[fauna_protocol::ext_kind::ExtKind],
    writer: Option<&[u8; 32]>,
) -> Result<GrantBlob, MintGrantError> {
    Ok(build_grant_blob_with_epochs(
        owner_actor_id,
        grant_id,
        holder_pubkey,
        holder_mlkem_ek,
        window,
        &ext_kinds_scope_wraps(delegable, kinds, writer),
    )?)
}

/// The tuples and wraps of [`mint_ext_kinds_grant`], unbuilt — so a consent
/// naming records and a folder read plans both into one grant
/// ([`ext_consent::prepare_ext_consent_grant`]).
#[must_use]
pub fn ext_kinds_scope_wraps(
    delegable: &fauna_core::crypto::DelegableSchedule,
    kinds: &[fauna_protocol::ext_kind::ExtKind],
    writer: Option<&[u8; 32]>,
) -> Vec<ScopeWraps> {
    let mut scopes = Vec::with_capacity(kinds.len() * 2);
    for kind in kinds {
        let kind = kind.to_string();
        let (entry_key, item_blind) = delegable.for_kind(&kind).to_grant();
        let mut payload = Vec::with_capacity(EXT_KIND_PAYLOAD_LEN);
        payload.extend_from_slice(&entry_key);
        payload.extend_from_slice(&item_blind);
        scopes.push((ScopeTuple::ext_kind_read(&kind), vec![(None, payload)]));
        if let Some(writer) = writer {
            scopes.push((ScopeTuple::content_write(&kind, writer), Vec::new()));
        }
    }
    scopes
}

/// The holder's side of [`mint_ext_kinds_grant`]: open every `ext.*` kind's
/// wrapped pair in `blob` with the holder's X25519 secret, one
/// [`fauna_core::crypto::AccountStateKindKeys`] per kind — what a principal
/// seals and opens its own rows with. A wrap that does not open, or a payload
/// of the wrong length, refuses the whole grant: a principal holding half of
/// what it was granted would seal rows nobody else can read.
///
/// # Errors
///
/// The wrap's own unseal error, or `InvalidFormat` for a malformed payload.
pub fn open_ext_kind_keys(
    blob: &GrantBlob,
    holder_x25519_secret: &[u8; 32],
) -> Result<Vec<fauna_core::crypto::AccountStateKindKeys>, fauna_mls::wrapped_blob::UnwrapError> {
    let owner: [u8; 32] = blob.index.0.as_slice().try_into().map_err(|_| {
        fauna_mls::wrapped_blob::UnwrapError::InvalidFormat("grant owner is not 32 bytes".into())
    })?;
    blob.wrapped_keys
        .iter()
        .filter(|w| {
            w.scope.class == ScopeTuple::CLASS_CONTENT_READ
                && w.scope
                    .kind
                    .as_deref()
                    .is_some_and(fauna_protocol::ext_kind::is_ext_kind)
        })
        .map(|w| {
            let payload =
                fauna_mls::wrapped_blob::unseal_capability(w, &owner, holder_x25519_secret)?;
            let kind = w.scope.kind.as_deref().unwrap_or_default();
            let pair: [u8; EXT_KIND_PAYLOAD_LEN] = payload.as_slice().try_into().map_err(|_| {
                fauna_mls::wrapped_blob::UnwrapError::InvalidFormat(format!(
                    "{kind}: the pair is {} bytes, not {EXT_KIND_PAYLOAD_LEN}",
                    payload.len()
                ))
            })?;
            let (entry_key, item_blind) = pair.split_at(32);
            Ok(fauna_core::crypto::AccountStateKindKeys::from_grant(
                kind,
                entry_key.try_into().expect("32"),
                item_blind.try_into().expect("32"),
            ))
        })
        .collect()
}

/// The keyless `identity.op` tuple for one operation class — what the user's
/// device declares in a grant to a third-party principal so the class's
/// custodian may perform it (`key-material-hierarchy.md` § Audience:
/// deployment infrastructure → *The oracle*: "the user's device, exactly like
/// a content grant"). The tuple's `kind` is the class's name; nothing else is
/// set, and [`derive_scope_payload`] wraps no key for it.
#[must_use]
pub fn identity_op_scope(class: fauna_core::identity_op::IdentityOpClass) -> ScopeTuple {
    ScopeTuple {
        class: ScopeTuple::CLASS_IDENTITY_OP.into(),
        kind: Some(class.name().into()),
        tier: None,
        set: None,
        factor: None,
    }
}

/// A capability grant could not be minted.
#[derive(Debug, thiserror::Error)]
pub enum MintGrantError {
    /// A declared scope tuple's payload could not be derived.
    #[error(transparent)]
    DerivePayload(#[from] DerivePayloadError),
    /// `build_grant_blob`'s HPKE seal step failed (e.g. a malformed holder
    /// pubkey).
    #[error(transparent)]
    Wrap(#[from] WrapError),
}

/// Assemble a user-minted [`GrantBlob`] from the owner's period-key and mail
/// custodies and a declared scope, ready for `to_canonical_bytes()` -> `fauna.capabilities.mint`
/// (design § 2.6, "the client-side mint — derive payloads + build
/// `GrantBlob`").
///
/// Derives each tuple's minimal payload via [`derive_scope_payload`],
/// bailing with the first [`DerivePayloadError`] encountered — callers that
/// want a partial grant (e.g. a settings page offering only the scopes the
/// owner can currently back) should filter `scope` themselves before calling
/// this, rather than relying on it to skip ungrantable tuples silently.
///
/// `holder_mlkem_ek` is the holder's published ML-KEM encapsulation key (from
/// `fauna.bridges.fetch_bridge_pubkey`), threaded straight into
/// [`build_grant_blob`]'s post-quantum wrap selector: `Some(valid ek)` ⇒ every
/// key-bearing tuple wraps X-Wing (so a harvested `capability_grants` row is not
/// a CRQC-openable bypass of the closed mail seal), `None` ⇒ the classical
/// X25519 wrap. The ek's presence is the whole gate — no capability token
/// (`post-quantum.md` § Capability-grant holders).
#[allow(clippy::too_many_arguments)]
pub fn mint_grant(
    period_keys: Option<&SubscriptionsConfig>,
    mail: &MailConfig,
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    window: GrantWindow,
    scope: &[ScopeTuple],
) -> Result<GrantBlob, MintGrantError> {
    let mut scopes = Vec::with_capacity(scope.len());
    for tuple in scope {
        let payload = derive_scope_payload(period_keys, mail, tuple)?;
        scopes.push((tuple.clone(), payload));
    }
    Ok(build_grant_blob(
        owner_actor_id,
        grant_id,
        holder_pubkey,
        holder_mlkem_ek,
        window,
        &scopes,
    )?)
}

/// Mint the web-paywall **folder** grant: `content.read{folder:set}` with
/// **one `WrappedScopeKey` per content-key generation**, `epoch` = the
/// generation `version` (`mls-group-key-material.md` § M2 third distribution
/// channel; behavior `monetization.md` § Pillar 2). Rotation appends the new
/// generation's wrap via `fauna.capabilities.renew` — re-mint with the grown
/// bundle and diff, or rebuild wholesale (idempotent for the holder).
///
/// `custody_channel_id` is the set's resolved custody identity — the serve
/// pseudo-channel (`fauna_core::folder_keys::serve_custody_channel_id`) for
/// an unshared paywalled set, or the derived real `ChannelId` for a shared
/// one. The caller (the `FoldersAuthor` orchestration, which holds the set
/// summary) resolves it via its `custody_channel_for` seam; this crate stays
/// summary-free and wasm-clean. Same-version duplicate generations (the
/// CRDT-merge edge, KMH § M2 *Generations*) each get their own wrap — the
/// holder keeps all candidates and the AEAD tag disambiguates at open.
///
/// # Errors
///
/// [`DerivePayloadError::FolderCustodyNotFound`] when no custody entry sits
/// at the resolved channel; [`MintGrantError::Wrap`] on a seal failure.
#[allow(clippy::too_many_arguments)] // mint_grant's flat shape + the two folder-specific args
pub fn mint_folder_grant(
    custody: &fauna_core::data::FoldersConfig,
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    window: GrantWindow,
    set_name: &str,
    custody_channel_id: &[u8; 32],
) -> Result<GrantBlob, MintGrantError> {
    Ok(build_grant_blob_with_epochs(
        owner_actor_id,
        grant_id,
        holder_pubkey,
        holder_mlkem_ek,
        window,
        &[folder_read_scope_wraps(
            custody,
            set_name,
            custody_channel_id,
        )?],
    )?)
}

/// The tuple and wraps of [`mint_folder_grant`], unbuilt — the folder read
/// twin a consent plans beside its other tuples
/// ([`ext_consent::prepare_ext_consent_grant`]).
///
/// # Errors
///
/// [`DerivePayloadError::FolderCustodyNotFound`] when no custody entry with
/// keys sits at `custody_channel_id`.
pub fn folder_read_scope_wraps(
    custody: &fauna_core::data::FoldersConfig,
    set_name: &str,
    custody_channel_id: &[u8; 32],
) -> Result<ScopeWraps, MintGrantError> {
    let keys = custody
        .sets
        .iter()
        .filter(|c| c.channel_id.as_ref() == Some(custody_channel_id))
        .min_by_key(|c| !c.is_live())
        .and_then(|c| c.keys.as_ref())
        .ok_or_else(|| DerivePayloadError::FolderCustodyNotFound(set_name.to_string()))?;
    let tuple = ScopeTuple::folder_read(&fauna_core::path_crypto::set_name_hash(set_name));
    let wraps = keys
        .generations()
        .map(|g| (Some(g.version), g.key.to_array().to_vec()))
        .collect();
    Ok((tuple, wraps))
}

/// The web-paywall capability grant id for a set at one generation (the
/// `GrantIndex` handle a `paywall_set` mint stamps). **Deterministic** —
/// derived from the owner's identity secret, the set name and the generation
/// — so the rotation (`FoldersAuthor::rotate_paywall_grant`) and revoke
/// (`FoldersAuthor::unpaywall_set`) legs can name the exact `(owner, grant_id)`
/// the mint used with **no persisted grant-id state**: the nest exposes no
/// owner-facing "list my grants" query (`fauna.capabilities.fetch` is
/// holder-scoped and returns sealed blobs), so a discarded random id would be
/// unrecoverable. Mint is `INSERT OR REPLACE` on `(owner, grant_id)`, so a
/// retry under the same derived id is idempotent (it replaces the row).
///
/// The generation is `webdav-server.md` § Key model → *A principal's read*
/// rule (1) → *The generation*, applied to the paywall grant: a `Revoke` is
/// terminal per id in the owner's log, so an unpaywall spends one generation
/// and the next paywall mints under the next. Callers never pick it
/// themselves: [`folder_paywall_generation`] walks the log to it.
///
/// Bound to `owner_secret` so it stays **unguessable to a peer** — the same
/// property a random id has (a predictable id must not let a peer target a
/// live grant), belt-and-suspenders atop the nest's already owner-scoped
/// renew/revoke handlers (the `(owner, grant_id)` PK is keyed on the
/// *authenticated* actor, so a guessed id matches zero of another owner's
/// rows). Every device of the one owner shares the identity secret, so all
/// agree on the id (multi-device consistent).
#[must_use]
pub fn folder_paywall_grant_id(
    owner_secret: &[u8; 32],
    set_name: &str,
    generation: u32,
) -> [u8; 16] {
    // Two fixed-width prefixes, so `name` needs no length tag. The domain is
    // `v2`: `v1` derived from `secret || name` alone, and a changed input
    // under one domain could collide with those gen-less ids.
    let mut material = Vec::with_capacity(32 + 4 + set_name.len());
    material.extend_from_slice(owner_secret);
    material.extend_from_slice(&generation.to_be_bytes());
    material.extend_from_slice(set_name.as_bytes());
    let full = blake3::derive_key("fauna.folders.paywall.grant_id.v2", &material);
    let mut id = [0u8; 16];
    id.copy_from_slice(&full[..16]);
    id
}

/// The grant id of a third-party principal's folder read twin over one set,
/// at one generation (`docs/goal/behavior/webdav-server.md` § Key model →
/// *A principal's read* rule (1), *Its grant shape* and *The generation*):
/// [`folder_paywall_grant_id`]'s derivation plus the holder, so each
/// (principal key, set) pair names its own grant from what every device of
/// the owner holds, and a `Revoke` — terminal per id in the owner's log —
/// spends one generation, never the pair. Callers never pick `generation`
/// themselves: [`folder_principal_generation`] walks the log to it.
#[must_use]
pub fn folder_principal_grant_id(
    owner_secret: &[u8; 32],
    holder: &[u8; 32],
    set_name: &str,
    generation: u32,
) -> [u8; 16] {
    // Three fixed-width prefixes, so `name` needs no length tag.
    let mut material = Vec::with_capacity(32 + 32 + 4 + set_name.len());
    material.extend_from_slice(owner_secret);
    material.extend_from_slice(holder);
    material.extend_from_slice(&generation.to_be_bytes());
    material.extend_from_slice(set_name.as_bytes());
    let full = blake3::derive_key("fauna.folders.principal.grant_id.v1", &material);
    let mut id = [0u8; 16];
    id.copy_from_slice(&full[..16]);
    id
}

/// Where the generation walk over one derived grant stopped
/// ([`derived_grant_generation`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedGrantGeneration {
    /// The first unspent generation — the one a mint goes to.
    pub generation: u32,
    /// That generation's id.
    pub grant_id: [u8; 16],
    /// The current grant under that id when it is live (a mint replaces it
    /// in place); `None` when the generation is fresh.
    pub live: Option<grant_log::CurrentGrant>,
}

/// The generation walk of `webdav-server.md` § Key model → *A principal's
/// read* rule (1) → *The generation*, over any generation-carrying derived
/// grant id — both the finder (renew, revoke) and the minter, so nothing else
/// derives the id. `id_of(n)` is the grant's id at generation `n`. For
/// `n = 0, 1, 2, …`: an id the log holds no event for is the next free
/// generation (nothing live); an id carrying a `Revoke` is spent (continue);
/// any other is the live generation, its current grant read through the
/// log's one fold ([`grant_log::current_grants`]).
///
/// Pure over the log: every device of the owner merges the same log by
/// union, so devices with merged logs agree on the generation.
#[must_use]
pub fn derived_grant_generation(
    events: &[fauna_core::grant_event::GrantEvent],
    id_of: impl Fn(u32) -> [u8; 16],
) -> DerivedGrantGeneration {
    let mut next = 0;
    for (generation, id, spent) in held_generations(events, &id_of) {
        if !spent {
            return DerivedGrantGeneration {
                generation,
                grant_id: id,
                live: fauna_core::grant_event::current_grants_of(events)
                    .into_iter()
                    .find(|g| g.grant_id == id),
            };
        }
        next = generation.saturating_add(1);
    }
    DerivedGrantGeneration {
        generation: next,
        grant_id: id_of(next),
        live: None,
    }
}

/// [`derived_grant_generation`] over a principal's folder grant
/// ([`folder_principal_grant_id`]).
#[must_use]
pub fn folder_principal_generation(
    events: &[fauna_core::grant_event::GrantEvent],
    owner_secret: &[u8; 32],
    holder: &[u8; 32],
    set_name: &str,
) -> DerivedGrantGeneration {
    derived_grant_generation(events, |g| {
        folder_principal_grant_id(owner_secret, holder, set_name, g)
    })
}

/// [`derived_grant_generation`] over a set's web-paywall grant
/// ([`folder_paywall_grant_id`]).
#[must_use]
pub fn folder_paywall_generation(
    events: &[fauna_core::grant_event::GrantEvent],
    owner_secret: &[u8; 32],
    set_name: &str,
) -> DerivedGrantGeneration {
    derived_grant_generation(events, |g| {
        folder_paywall_grant_id(owner_secret, set_name, g)
    })
}

/// The generations of one derived grant the log holds an event for, in order
/// — `(generation, id, spent)`, `spent` when the id carries a `Revoke` —
/// ending at the first id with no event: the one step
/// [`derived_grant_generation`] and the folder resolvers
/// ([`folder_principal_set_names`], [`folder_paywall_set_names`]) all walk.
fn held_generations<'a, F: Fn(u32) -> [u8; 16]>(
    events: &'a [fauna_core::grant_event::GrantEvent],
    id_of: &'a F,
) -> impl Iterator<Item = (u32, [u8; 16], bool)> + 'a {
    // Each held generation carries an event, so the walk stops by
    // `events.len()`; the bound only keeps the range total.
    (0..u32::MAX).map_while(move |generation| {
        let id = id_of(generation);
        let mut of_id = events.iter().filter(|e| e.grant_id == id).peekable();
        of_id.peek()?;
        let spent = of_id.any(|e| e.kind == fauna_core::grant_event::GrantEventKind::Revoke);
        Some((generation, id, spent))
    })
}

/// Every id the log holds for `id_of(set, ·)` over a set in `set_names`, at
/// every generation — spent ones included, so a History-lens `Revoke` names
/// its folder — mapped to that set's name: the walk both folder resolvers
/// share.
fn derived_set_names(
    events: &[fauna_core::grant_event::GrantEvent],
    set_names: &[String],
    id_of: impl Fn(&str, u32) -> [u8; 16],
) -> std::collections::BTreeMap<[u8; 16], String> {
    let mut named = std::collections::BTreeMap::new();
    for set_name in set_names {
        let of_set = |g| id_of(set_name, g);
        for (_, id, _) in held_generations(events, &of_set) {
            named.insert(id, set_name.clone());
        }
    }
    named
}

/// Which folder each of `holder`'s principal folder grants covers
/// (`webdav-server.md` § Key model → *A principal's read* rule (1): the
/// signed `GrantEvent` carries no set, so the walk over the owner's sets is
/// the only answer): every grant id the log holds for `holder` over a set in
/// `set_names`, at every generation — spent ones included, so a History-lens
/// `Revoke` names its folder — mapped to that set's name. A folder grant
/// whose id is absent covers a set the owner no longer has.
#[must_use]
pub fn folder_principal_set_names(
    events: &[fauna_core::grant_event::GrantEvent],
    owner_secret: &[u8; 32],
    holder: &[u8; 32],
    set_names: &[String],
) -> std::collections::BTreeMap<[u8; 16], String> {
    derived_set_names(events, set_names, |set, g| {
        folder_principal_grant_id(owner_secret, holder, set, g)
    })
}

/// Which folder each of the owner's web-paywall grants covers —
/// [`folder_principal_set_names`] over [`folder_paywall_grant_id`]: every
/// paywall grant id the log holds over a set in `set_names`, at every
/// generation, mapped to that set's name. A paywall grant whose id is absent
/// covers a set the owner no longer has.
#[must_use]
pub fn folder_paywall_set_names(
    events: &[fauna_core::grant_event::GrantEvent],
    owner_secret: &[u8; 32],
    set_names: &[String],
) -> std::collections::BTreeMap<[u8; 16], String> {
    derived_set_names(events, set_names, |set, g| {
        folder_paywall_grant_id(owner_secret, set, g)
    })
}

/// Every principal's live folder read grant over `set_name` — rule (4)'s
/// finder (`webdav-server.md` § Key model → *A principal's read*): for each
/// holder the log holds a current grant for, [`folder_principal_generation`]'s
/// `live`. A holder with no twin over the set walks to a fresh generation and
/// adds nothing, so the paywall grant, the records grant and another set's
/// twin are never named. Ordered by holder, one grant per holder at most.
#[must_use]
pub fn live_folder_principal_grants(
    events: &[fauna_core::grant_event::GrantEvent],
    owner_secret: &[u8; 32],
    set_name: &str,
) -> Vec<grant_log::CurrentGrant> {
    let holders: std::collections::BTreeSet<[u8; 32]> =
        fauna_core::grant_event::current_grants_of(events)
            .iter()
            .filter_map(|g| <[u8; 32]>::try_from(g.holder.as_slice()).ok())
            .collect();
    holders
        .iter()
        .filter_map(|holder| {
            folder_principal_generation(events, owner_secret, holder, set_name).live
        })
        .collect()
}

/// The set names the owner's folder grant ids are matched over — the input
/// [`folder_principal_set_names`] and [`folder_paywall_set_names`] both walk.
/// A seam because the names rest in folder-key custody beside the nest's
/// owner-scoped folder list, neither of which this crate reads; the custody
/// fold that answers it is `fauna_client_folders::CustodyOwnedSetNames`.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait OwnedSetNames: fauna_core::MaybeSendSync {
    /// The set name of every folder the owner owns. `None` when custody or
    /// the folder list cannot be read now — then no grant names a folder,
    /// rather than every folder grant reading as a deleted set.
    async fn owned_set_names(&self) -> Option<Vec<String>>;
}

/// Mint a **bounded** (crypto-time-boxed) mail grant: `content.read{mail}`
/// carrying **one `WrappedScopeKey` per sealing epoch** whose time-range
/// intersects `window` (`epoch = Some(e)`,
/// [`derive_recipient_mail_epoch_capability_secret`] payloads) — and **never**
/// the standing secret. This is the grant shape whose expiry cryptographically
/// binds future content once the mail ingest cutover is live: a holder that
/// kept every wrapped key it ever fetched still cannot open content sealed in
/// an epoch outside its windows (content-sealing-epochs design 2026-07-18
/// § 2). The mixed bounded+standing shape is a mint-side error, enforced in
/// the shared blob builder.
///
/// `include_label_write` adds the keyless `content.label-write` tuple — the
/// background scorer's composed role (`content.read{mail}` + label-write,
/// design § taxonomy). For the legacy-backlog / serve use cases that need the
/// standing key, mint a master-key grant via [`mint_grant`] instead — the
/// settings page states the honest bound per regime.
///
/// # Errors
///
/// [`DerivePayloadError::MailNotEnabled`] when the mail custody's `msek` is
/// unset; [`MintGrantError::Wrap`] on a seal failure.
pub fn mint_bounded_mail_grant(
    mail: &MailConfig,
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    window: GrantWindow,
    include_label_write: bool,
) -> Result<GrantBlob, MintGrantError> {
    let (msek, priors) = mail_msek_generations(mail)?;
    Ok(build_bounded_mail_grant(
        owner_actor_id,
        grant_id,
        holder_pubkey,
        holder_mlkem_ek,
        window,
        &msek,
        &priors,
        include_label_write,
        None,
    )?)
}

/// Mint the **per-labeler** bounded mail grant — what subscribing a `wasm`
/// mail labeler over sealed mail IS (`content-moderation-and-ranking.md`
/// § Tier-3 → *Subscribing = minting a capability*; the subscription row
/// links to it 1:1 by `grant_id`). The same epoch-wrapped
/// `content.read{mail}` payloads as [`mint_bounded_mail_grant`], but every
/// tuple and every wrap carries `factor = labeler:<hex>`
/// (`fauna_core::scoring::labeler_factor`), so the holder's wraps open the
/// owner's mail only for that labeler's score and its label-write licenses
/// only that factor; the composed "read and filter my mail" grant licenses
/// no community labeler at all. Unsubscribe = revoke this grant (the drain
/// goes dark for that labeler alone).
///
/// # Errors
///
/// [`DerivePayloadError::MailNotEnabled`] when the mail custody's `msek` is
/// unset; [`MintGrantError::Wrap`] on a seal failure.
pub fn mint_bounded_mail_labeler_grant(
    mail: &MailConfig,
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; 16],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    window: GrantWindow,
    labeler_id: &fauna_core::identity::ActorId,
) -> Result<GrantBlob, MintGrantError> {
    let (msek, priors) = mail_msek_generations(mail)?;
    let factor = fauna_core::scoring::labeler_factor(labeler_id);
    Ok(build_bounded_mail_grant(
        owner_actor_id,
        grant_id,
        holder_pubkey,
        holder_mlkem_ek,
        window,
        &msek,
        &priors,
        true,
        Some(&factor),
    )?)
}

/// The owner's MSEK generations for a bounded-mail mint: the current MSEK
/// plus each retained prior generation with its recorded retirement instant.
///
/// Every rotation records the instant in the same write that retains the
/// generation, and the mail custody's merge prunes the two together, so a prior
/// with no instant is an inconsistent config, not a shape any writer produces.
/// It contributes no epochs (its seal interval is unknowable, and guessing one
/// would over-grant). The pre-2026-07-19 "legacy retention" reading of such a
/// prior was retired by the compat-remnant sweep (`version-compatibility.md`
/// § Dimension 2, program 4).
///
/// # Errors
///
/// [`DerivePayloadError::MailNotEnabled`] when the mail custody's `msek` is
/// unset.
fn mail_msek_generations(
    mail: &MailConfig,
) -> Result<([u8; 32], Vec<PriorMsekGeneration>), DerivePayloadError> {
    let msek = mail
        .msek
        .as_ref()
        .ok_or(DerivePayloadError::MailNotEnabled)?;
    // `PriorMsekGeneration` is a *mint argument*, not a carrier: the bounded-mail
    // mint consumes it to derive an epoch root and drops it. Copying the bytes out
    // of custody here is the same boundary the *Carrier shape* rule already
    // declares out of scope for the content-key family, not a missed member of the
    // `MailConfig` family this pass flipped.
    let priors = mail
        .prior_mseks
        .iter()
        .filter_map(|m| {
            Some(PriorMsekGeneration {
                msek: m.to_array(),
                retired_at_unix: mail.prior_msek_retired_at(m)?,
            })
        })
        .collect();
    Ok((msek.to_array(), priors))
}

/// The `appended_keys` a `fauna.capabilities.renew` carries when extending a
/// **bounded** mail grant's window from `old_end_unix` to `new_end_unix`:
/// one per-epoch wrap for each sealing epoch newly covered by the extension
/// (`epoch_of(old_end) + 1 ..= epoch_of(new_end)`). Empty when the extension
/// stays inside the epoch already covered — the renew is then a pure window
/// bump. The nest dedups appends by `(scope, epoch)`, so resending an
/// already-held epoch is harmless (idempotent retries).
///
/// `factor` is the grant's own confinement — `Some("labeler:<hex>")` for a
/// per-labeler grant ([`mint_bounded_mail_labeler_grant`]), `None` for the
/// composed MDA role — and every appended wrap carries it, exactly as the
/// mint's did: a factor-less wrap appended to a per-labeler grant would widen
/// it to the composed read. Read it off the log with
/// `grant_log::labeler_factor_of_grant`.
///
/// # Errors
///
/// [`DerivePayloadError::MailNotEnabled`] when the mail custody's `msek` is
/// unset; [`MintGrantError::Wrap`] on a seal failure.
pub fn bounded_mail_renewal_keys(
    mail: &MailConfig,
    owner_actor_id: &[u8; 32],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    old_end_unix: u64,
    new_end_unix: u64,
    factor: Option<&str>,
) -> Result<Vec<WrappedScopeKey>, MintGrantError> {
    let (msek, priors) = mail_msek_generations(mail)?;
    let first_new = mail_sealing_epoch_of(old_end_unix) + 1;
    let last = mail_sealing_epoch_of(new_end_unix);
    if first_new > last {
        return Ok(Vec::new());
    }
    let wraps = bounded_mail_epoch_wraps_for_range(&msek, &priors, first_new..=last);
    Ok(build_renewal_wraps(
        owner_actor_id,
        holder_pubkey,
        holder_mlkem_ek,
        &[(mail_scope_tuple(factor), wraps)],
    )?)
}

/// The `appended_keys` that **heal** an outstanding bounded mail grant after
/// an MSEK hard-revoke (content-sealing-epochs § 5 + the 2026-07-19
/// amendment): a full re-wrap of every epoch in the grant's window under the
/// current generation set — boundary epochs concatenate the retired
/// generation's secret inside the single per-epoch payload, so the nest's
/// replace-on-append (`fauna.capabilities.renew`) is lossless: post-rotation
/// content becomes openable without pre-rotation content going dark. Send as
/// `RenewGrantRequest.appended_keys` with `new_epoch_end = window.1`
/// (an equal-end renew is a pure key refresh; shrinking is refused
/// nest-side).
///
/// The rotation-flow driver that enumerates outstanding bounded grants and
/// calls this is follow-on work;
/// the helper is the shared-Rust half every app will share.
///
/// `factor` is the grant's own confinement, carried onto every healed wrap
/// (see [`bounded_mail_renewal_keys`]).
///
/// # Errors
///
/// [`DerivePayloadError::MailNotEnabled`] when the mail custody's `msek` is
/// unset; [`MintGrantError::Wrap`] on a seal failure.
pub fn bounded_mail_rotation_heal_keys(
    mail: &MailConfig,
    owner_actor_id: &[u8; 32],
    holder_pubkey: &[u8; 32],
    holder_mlkem_ek: Option<&[u8]>,
    window: &GrantWindow,
    factor: Option<&str>,
) -> Result<Vec<WrappedScopeKey>, MintGrantError> {
    let (msek, priors) = mail_msek_generations(mail)?;
    let wraps = bounded_mail_epoch_wraps(&msek, &priors, window);
    Ok(build_renewal_wraps(
        owner_actor_id,
        holder_pubkey,
        holder_mlkem_ek,
        &[(mail_scope_tuple(factor), wraps)],
    )?)
}

/// The `content.read{mail}` scope tuple, confined to `factor` when the grant
/// is (`ScopeTuple::mail_for_factor`).
fn mail_scope_tuple(factor: Option<&str>) -> ScopeTuple {
    match factor {
        Some(f) => ScopeTuple::mail_for_factor(f),
        None => ScopeTuple::mail(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::MailConfig;
    use fauna_mls::wrapped_blob::{
        FAUNA_KEM_XWING, MLKEM768_DECAPS_KEY_LEN, derive_bridge_service_user_mlkem768,
        derive_recipient_hpke_keypair, derive_recipient_xwing_keypair, generate_x25519_keypair,
        unseal_capability, unseal_capability_hybrid,
    };

    fn mail_scope() -> ScopeTuple {
        mail_scope_tuple(None)
    }

    fn spam_model_scope() -> ScopeTuple {
        ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_SPAM_MODEL.into()),
            tier: None,
            set: None,
            factor: None,
        }
    }

    fn post_scope(tier: &str) -> ScopeTuple {
        ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_POST.into()),
            tier: Some(tier.into()),
            set: None,
            factor: None,
        }
    }

    fn label_write_scope() -> ScopeTuple {
        ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_LABEL_WRITE.into(),
            kind: None,
            tier: None,
            set: None,
            factor: None,
        }
    }

    #[test]
    fn mail_scope_without_msek_reports_mail_not_enabled() {
        let mail = MailConfig::default();
        assert_eq!(
            derive_scope_payload(None, &mail, &mail_scope()),
            Err(DerivePayloadError::MailNotEnabled)
        );
    }

    #[test]
    fn mail_scope_derives_xwing_superset_payload_from_msek() {
        let mut mail = MailConfig::default();
        // The mail/calendar payload is the `32 + 2400`-byte X-Wing shape
        // (`x25519_secret ∥ ml-kem-dk`, `post-quantum.md` § surface A). Its first
        // 32 bytes ARE the classical recipient secret (a pure superset), so a
        // holder opens both classical and hybrid mail records with it.
        let msek = [0x42u8; 32];
        mail.msek = Some(msek.into());
        let payload = derive_scope_payload(None, &mail, &mail_scope())
            .unwrap()
            .expect("mail is key-bearing");
        assert_eq!(payload.len(), 32 + MLKEM768_DECAPS_KEY_LEN);
        // Superset: the X25519 half is the classical recipient secret verbatim.
        let (expected_secret, _) = derive_recipient_hpke_keypair(&msek);
        assert_eq!(&payload[..32], &expected_secret[..]);
        // The ML-KEM half is the recipient's derived decaps key.
        let xwing = derive_recipient_xwing_keypair(&msek);
        assert_eq!(&payload[32..], xwing.secret.mlkem_decaps_key());
    }

    #[test]
    fn calendar_scope_derives_the_same_xwing_superset() {
        // calendar reuses the mail recipient key (same MSEK-derived seal key).
        let mail = MailConfig {
            msek: Some([0x42u8; 32].into()),
            ..MailConfig::default()
        };
        let cal = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_CALENDAR.into()),
            tier: None,
            set: None,
            factor: None,
        };
        let payload = derive_scope_payload(None, &mail, &cal)
            .unwrap()
            .expect("calendar is key-bearing");
        assert_eq!(payload.len(), 32 + MLKEM768_DECAPS_KEY_LEN);
    }

    #[test]
    fn post_scope_without_tier_field_errors() {
        let mail = MailConfig::default();
        let tuple = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_POST.into()),
            tier: None,
            set: None,
            factor: None,
        };
        assert_eq!(
            derive_scope_payload(None, &mail, &tuple),
            Err(DerivePayloadError::PostMissingTier)
        );
    }

    #[test]
    fn post_scope_without_recorded_period_key_errors() {
        let mail = MailConfig::default();
        let custody = fauna_core::data::SubscriptionsConfig::default();
        assert_eq!(
            derive_scope_payload(Some(&custody), &mail, &post_scope("gold")),
            Err(DerivePayloadError::TierNotFound("gold".into()))
        );
    }

    /// A caller that read no period-key custody is refused a post tier as
    /// such — never answered as "no key held", which would read as a tier
    /// the author never created.
    #[test]
    fn post_scope_without_read_custody_is_refused_as_unread() {
        assert_eq!(
            derive_scope_payload(None, &MailConfig::default(), &post_scope("gold")),
            Err(DerivePayloadError::PeriodKeysUnread("gold".into()))
        );
    }

    #[test]
    fn post_scope_derives_current_period_key() {
        let mail = MailConfig::default();
        let mut custody = fauna_core::data::SubscriptionsConfig::default();
        fauna_client_subscriptions::custody::record_new_tier(
            &mut custody,
            fauna_core::identity::ActorId([9u8; 32]),
            "gold",
            [0x11; 32],
            1000,
        );
        let payload = derive_scope_payload(Some(&custody), &mail, &post_scope("gold"))
            .unwrap()
            .expect("post is key-bearing");
        assert_eq!(payload, vec![0x11u8; 32]);
    }

    /// TP11: every built `identity.op` class mints keyless, and a tuple
    /// naming a sovereign operation — or no built class — is refused at the
    /// mint, so no such grant ever reaches a nest.
    #[test]
    fn identity_op_scope_is_keyless_and_the_deny_list_binds_at_the_mint() {
        use fauna_core::identity_op::{IdentityOpClass, SovereignOp};
        let mail = MailConfig::default();
        for class in IdentityOpClass::ALL {
            let tuple = identity_op_scope(*class);
            assert_eq!(tuple.class, ScopeTuple::CLASS_IDENTITY_OP);
            assert_eq!(tuple.kind.as_deref(), Some(class.name()));
            assert_eq!(derive_scope_payload(None, &mail, &tuple), Ok(None));
        }
        for op in SovereignOp::ALL {
            let mut tuple = identity_op_scope(IdentityOpClass::NostrSignEvent);
            tuple.kind = Some(op.name().into());
            match derive_scope_payload(None, &mail, &tuple) {
                Err(DerivePayloadError::IdentityOpRefused(why)) => {
                    assert!(why.contains("sovereign"), "{why}")
                }
                other => panic!("{}: expected a sovereign refusal, got {other:?}", op.name()),
            }
        }
        let mut kindless = identity_op_scope(IdentityOpClass::NostrSignEvent);
        kindless.kind = None;
        assert!(matches!(
            derive_scope_payload(None, &mail, &kindless),
            Err(DerivePayloadError::IdentityOpRefused(_))
        ));
    }

    #[test]
    fn label_write_scope_is_keyless() {
        let mail = MailConfig::default();
        assert_eq!(
            derive_scope_payload(None, &mail, &label_write_scope()),
            Ok(None)
        );
    }

    #[test]
    fn spam_model_scope_is_keyless_even_with_mail_enabled() {
        // The ratified R2 (account-data-plane.md § The ratified decisions) shape (`mail-spam.md` § Encrypted-mode interaction,
        // 2026-07-13): even when MSEK is present — i.e. the mail arm above it
        // COULD derive the recipient-mail secret — the spam-model kind must
        // never receive a payload; the grant is audit/authorization only and
        // the model travels as a sealed-to-holder copy.
        let mail = MailConfig {
            msek: Some([0x42u8; 32].into()),
            ..MailConfig::default()
        };
        assert_eq!(
            derive_scope_payload(None, &mail, &spam_model_scope()),
            Ok(None)
        );
    }

    #[test]
    fn mint_grant_spam_model_conveys_no_key_material() {
        let mut mail = MailConfig::default();
        // Definition-of-success negative (mint layer): a contributor's
        // spam-model grant demonstrably cannot open their mail — it carries
        // ZERO wrapped keys, while the same mint with a mail scope would have
        // carried the recipient-mail secret.
        let custody = fauna_core::data::SubscriptionsConfig::default();
        mail.msek = Some([0x42u8; 32].into());
        let (_, holder_pk) = generate_x25519_keypair();
        let owner = [0x77u8; 32];
        let grant_id = [0x02u8; 16];

        let blob = mint_grant(
            Some(&custody),
            &mail,
            &owner,
            &grant_id,
            &holder_pk,
            None,
            GrantWindow(0, DEFAULT_GRANT_WINDOW_SECS),
            &[spam_model_scope()],
        )
        .unwrap();
        assert_eq!(blob.scope, vec![spam_model_scope()]);
        assert!(
            blob.wrapped_keys.is_empty(),
            "spam-model grant must convey no key material"
        );

        // Mixed mint: the mail tuple wraps its key, the spam-model tuple
        // stays keyless — kinds never share a wrap.
        let blob = mint_grant(
            Some(&custody),
            &mail,
            &owner,
            &grant_id,
            &holder_pk,
            None,
            GrantWindow(0, DEFAULT_GRANT_WINDOW_SECS),
            &[mail_scope(), spam_model_scope()],
        )
        .unwrap();
        assert_eq!(blob.scope.len(), 2);
        assert_eq!(blob.wrapped_keys.len(), 1);
        assert_eq!(blob.wrapped_keys[0].scope, mail_scope());
    }

    #[test]
    fn index_scopes_report_not_yet_implemented() {
        let mail = MailConfig::default();
        for class in [ScopeTuple::CLASS_INDEX_WRITE, ScopeTuple::CLASS_INDEX_READ] {
            let tuple = ScopeTuple {
                class: class.into(),
                kind: Some(ScopeTuple::KIND_MAIL.into()),
                tier: None,
                set: None,
                factor: None,
            };
            assert_eq!(
                derive_scope_payload(None, &mail, &tuple),
                Err(DerivePayloadError::IndexNotImplemented)
            );
        }
    }

    #[test]
    fn a_deposit_tuple_is_keyless_over_exactly_one_folder() {
        let mail = MailConfig::default();
        assert_eq!(
            derive_scope_payload(None, &mail, &ScopeTuple::folder_deposit(42)),
            Ok(None)
        );
        for (set, kind) in [
            (None, None),
            (Some("x"), None),
            (Some("042"), None),
            (Some("42"), Some("mail")),
        ] {
            let tuple = ScopeTuple {
                class: ScopeTuple::CLASS_DEPOSIT.into(),
                kind: kind.map(str::to_string),
                tier: None,
                set: set.map(str::to_string),
                factor: None,
            };
            assert!(
                matches!(
                    derive_scope_payload(None, &mail, &tuple),
                    Err(DerivePayloadError::DepositRefused(_))
                ),
                "{tuple:?} must be refused"
            );
        }
    }

    #[test]
    fn a_folder_deposit_grant_wraps_nothing() {
        let blob =
            mint_folder_deposit_grant(&[1; 32], &[2; 16], &[3; 32], GrantWindow(0, 100), &[7, 9])
                .expect("mints");
        assert!(blob.wrapped_keys.is_empty(), "deposit is keyless");
        assert_eq!(
            blob.scope,
            vec![ScopeTuple::folder_deposit(7), ScopeTuple::folder_deposit(9)]
        );
    }

    #[test]
    fn unknown_class_and_kind_are_reported() {
        let mail = MailConfig::default();
        let unknown_class = ScopeTuple {
            class: "content.delete".into(),
            kind: None,
            tier: None,
            set: None,
            factor: None,
        };
        assert_eq!(
            derive_scope_payload(None, &mail, &unknown_class),
            Err(DerivePayloadError::UnknownClass("content.delete".into()))
        );

        let unknown_kind = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some("conversation".into()),
            tier: None,
            set: None,
            factor: None,
        };
        assert_eq!(
            derive_scope_payload(None, &mail, &unknown_kind),
            Err(DerivePayloadError::UnknownReadKind(Some(
                "conversation".into()
            )))
        );
    }

    #[test]
    fn mint_grant_builds_a_grant_blob_from_userconfig() {
        let mut mail = MailConfig::default();
        let mut custody = fauna_core::data::SubscriptionsConfig::default();
        let msek = [0x42u8; 32];
        mail.msek = Some(msek.into());
        fauna_client_subscriptions::custody::record_new_tier(
            &mut custody,
            fauna_core::identity::ActorId([9u8; 32]),
            "gold",
            [0x11; 32],
            1000,
        );

        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x77u8; 32];
        let grant_id = [0x01u8; 16];
        let scope = vec![mail_scope(), post_scope("gold"), label_write_scope()];

        let blob = mint_grant(
            Some(&custody),
            &mail,
            &owner,
            &grant_id,
            &holder_pk,
            None, // classical wrap: holder published no ML-KEM ek
            GrantWindow(0, DEFAULT_GRANT_WINDOW_SECS),
            &scope,
        )
        .unwrap();

        assert_eq!(blob.scope.len(), 3);
        assert_eq!(blob.wrapped_keys.len(), 2, "label-write is keyless");

        let mail_key = blob
            .wrapped_keys
            .iter()
            .find(|k| k.scope == mail_scope())
            .expect("mail wrapped key present");
        // Classical wrap; the payload is the 2432-byte X-Wing superset, whose
        // first 32 bytes are the classical recipient secret.
        assert_eq!(
            mail_key.hpke.kem_suite,
            fauna_mls::wrapped_blob::KemSuite::STANDARD
        );
        let opened = unseal_capability(mail_key, &owner, &holder_sk).unwrap();
        assert_eq!(opened.len(), 32 + MLKEM768_DECAPS_KEY_LEN);
        let (expected_mail_secret, _) = derive_recipient_hpke_keypair(&msek);
        assert_eq!(&opened[..32], &expected_mail_secret[..]);
    }

    #[test]
    fn mint_grant_with_holder_ek_wraps_xwing_and_opens_hybrid() {
        let mut mail = MailConfig::default();
        // PQ-CAP-3 end-to-end at the mint layer: a holder that published a
        // valid ML-KEM ek gets the grant wrapped X-Wing, and opens it back with
        // its X25519 secret + derived ML-KEM dk (the drain rendezvous shape).
        let custody = fauna_core::data::SubscriptionsConfig::default();
        let msek = [0x42u8; 32];
        mail.msek = Some(msek.into());

        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let (holder_dk, holder_ek) = derive_bridge_service_user_mlkem768(b"a-bridge-seed");
        let owner = [0x77u8; 32];
        let scope = vec![mail_scope()];

        let blob = mint_grant(
            Some(&custody),
            &mail,
            &owner,
            &[0x02u8; 16],
            &holder_pk,
            Some(holder_ek.as_slice()),
            GrantWindow(0, DEFAULT_GRANT_WINDOW_SECS),
            &scope,
        )
        .unwrap();

        let mail_key = &blob.wrapped_keys[0];
        assert_eq!(mail_key.hpke.kem_suite.kem, FAUNA_KEM_XWING);
        let opened = unseal_capability_hybrid(mail_key, &owner, &holder_sk, &holder_dk).unwrap();
        // The full X-Wing superset payload — the drain feeds this to
        // `open_mail_record_with_key` to open hybrid mail.
        assert_eq!(opened.len(), 32 + MLKEM768_DECAPS_KEY_LEN);
        let (expected_mail_secret, _) = derive_recipient_hpke_keypair(&msek);
        assert_eq!(&opened[..32], &expected_mail_secret[..]);
    }

    /// The `ext.*` grant's two classes on the generic path: `content.write` is
    /// keyless and shape-checked; `content.read` over an `ext.*` kind points at
    /// the dedicated mint (`third-party-kinds.md` § Principal write authority).
    #[test]
    fn content_write_is_keyless_and_an_ext_read_takes_the_dedicated_mint() {
        let mail = MailConfig::default();
        let writer = [0x5Au8; 32];
        let kind = "ext.example.com.notes";
        assert_eq!(
            derive_scope_payload(None, &mail, &ScopeTuple::content_write(kind, &writer)),
            Ok(None)
        );
        assert_eq!(
            derive_scope_payload(None, &mail, &ScopeTuple::ext_kind_read(kind)),
            Err(DerivePayloadError::ExtKindUsesDedicatedMint)
        );
        for refused in [
            // Not an ext.* kind.
            ScopeTuple::content_write(ScopeTuple::KIND_MAIL, &writer),
            // No writer: whose writing would it authorize?
            ScopeTuple {
                factor: None,
                ..ScopeTuple::content_write(kind, &writer)
            },
            // A labeler's factor is no writer key.
            ScopeTuple {
                factor: Some(format!("labeler:{}", "ab".repeat(32))),
                ..ScopeTuple::content_write(kind, &writer)
            },
        ] {
            assert!(
                matches!(
                    derive_scope_payload(None, &mail, &refused),
                    Err(DerivePayloadError::ContentWriteRefused(_))
                ),
                "{refused:?}"
            );
        }
    }

    /// **The scope-narrowness pin** (`third-party-kinds.md` § The `ext`
    /// sub-scope + § Principal write authority): a grant minted over
    /// `ext.example.com.*` — expanded to the manifest's kinds — to a
    /// principal's key opens rows of exactly those kinds; a row under
    /// `ext.other.org.thing`, sealed by the same account, opens under none of
    /// the granted pairs. The `content.write` tuples wrap nothing and carry
    /// the attested writer, which the grant log records as the replica-side
    /// admission's input.
    #[test]
    fn an_ext_grant_opens_only_its_publishers_kinds() {
        use fauna_core::account_entry_crypto::{
            EntryCoordinates, EntryPlaintext, open_entry, seal_entry,
        };
        use fauna_core::crypto::{AccountStateKeySchedule, BackupKey};
        use fauna_protocol::ext_kind::ExtKind;

        let schedule = AccountStateKeySchedule::derive(&BackupKey::derive(&[7u8; 32]));
        let device = ed25519_dalek::SigningKey::from_bytes(&[0xD0; 32]);
        let owner = [0x77u8; 32];
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let writer = [0x5Au8; 32];
        let kinds: Vec<ExtKind> = ["ext.example.com.notes", "ext.example.com.todo"]
            .iter()
            .map(|k| k.parse().unwrap())
            .collect();

        let blob = mint_ext_kinds_grant(
            schedule.delegable(),
            &owner,
            &[0x03u8; 16],
            &holder_pk,
            None,
            GrantWindow(0, DEFAULT_GRANT_WINDOW_SECS),
            &kinds,
            Some(&writer),
        )
        .unwrap();
        assert_eq!(blob.wrapped_keys.len(), 2, "one wrap per kind's read tuple");
        let writes: Vec<_> = blob
            .scope
            .iter()
            .filter(|t| t.class == ScopeTuple::CLASS_CONTENT_WRITE)
            .collect();
        assert_eq!(writes.len(), 2, "one keyless write tuple per kind");
        assert!(
            blob.wrapped_keys
                .iter()
                .all(|w| w.scope.class == ScopeTuple::CLASS_CONTENT_READ)
        );
        let authority = fauna_core::grant_event::content_write_authorizations(&[
            fauna_core::grant_event::GrantEvent {
                grant_id: vec![3; 16],
                holder: holder_pk.to_vec(),
                kind: fauna_core::grant_event::GrantEventKind::Mint,
                scope: grant_log::event_scope_of(&blob.scope),
                window_start: 0,
                window_end: DEFAULT_GRANT_WINDOW_SECS,
                at: 1,
                sig: Vec::new(),
            },
        ]);
        assert_eq!(authority.len(), 2);
        assert!(authority.values().all(|w| w.contains(&writer)));

        let granted = open_ext_kind_keys(&blob, &holder_sk).unwrap();
        assert_eq!(granted.len(), 2);

        let opens = |kind: &str| {
            let scope = format!("ext:{kind}");
            let coords = EntryCoordinates {
                writer_id: device.verifying_key().to_bytes(),
                writer_seq: 1,
                scope: &scope,
            };
            let plaintext = EntryPlaintext {
                kind: kind.into(),
                key: "k".into(),
                merge_meta: None,
                value: fauna_protocol::ByteBuf::from(b"v".to_vec()),
                tombstone: false,
            };
            let keys = schedule.delegable().for_kind(kind).into_keys();
            let sealed = seal_entry(&keys, &coords, &plaintext, &device).unwrap();
            granted
                .iter()
                .any(|keys| open_entry(keys, &coords, &sealed.item_key, &sealed.envelope).is_ok())
        };
        assert!(opens("ext.example.com.notes"));
        assert!(opens("ext.example.com.todo"));
        assert!(
            !opens("ext.other.org.thing"),
            "a sibling publisher's row must not open under the granted pairs"
        );
        assert!(
            !opens("ext.example.com.other"),
            "nor a kind the manifest never declared"
        );
    }

    #[test]
    fn mint_grant_propagates_derive_payload_error() {
        let mail = MailConfig::default();
        let (_holder_sk, holder_pk) = generate_x25519_keypair();
        let err = mint_grant(
            None,
            &mail,
            &[0x77u8; 32],
            &[0x01u8; 16],
            &holder_pk,
            None,
            GrantWindow(0, DEFAULT_GRANT_WINDOW_SECS),
            &[mail_scope()],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            MintGrantError::DerivePayload(DerivePayloadError::MailNotEnabled)
        ));
    }

    #[test]
    fn folder_scope_in_generic_mint_reports_dedicated_path() {
        let mail = MailConfig::default();
        let tuple = ScopeTuple {
            class: ScopeTuple::CLASS_CONTENT_READ.into(),
            kind: Some(ScopeTuple::KIND_FOLDER.into()),
            tier: None,
            set: Some("site".into()),
            factor: None,
        };
        assert_eq!(
            derive_scope_payload(None, &mail, &tuple),
            Err(DerivePayloadError::FolderUsesDedicatedMint)
        );
    }

    #[test]
    fn mint_folder_grant_wraps_every_generation_by_version() {
        use fauna_core::data::FolderKeyCustody;
        use fauna_core::folder_keys::{FolderContentKeys, serve_custody_channel_id};

        let mut cfg = fauna_core::data::FoldersConfig::default();
        let channel = serve_custody_channel_id("members-site");
        let mut keys = FolderContentKeys::genesis([0xC1u8; 32], 1_000);
        keys.rotate([0xC2u8; 32], 2_000); // version 2
        cfg.sets.push(FolderKeyCustody {
            channel_id: Some(channel),
            keys: Some(keys),
            ..Default::default()
        });

        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let owner = [0x88u8; 32];
        let blob = mint_folder_grant(
            &cfg,
            &owner,
            &[0x02u8; 16],
            &holder_pk,
            None,
            GrantWindow(0, DEFAULT_GRANT_WINDOW_SECS),
            "members-site",
            &channel,
        )
        .unwrap();

        assert_eq!(blob.scope.len(), 1);
        assert_eq!(blob.scope[0].kind.as_deref(), Some("folder"));
        assert_eq!(
            blob.scope[0].set,
            Some(ScopeTuple::folder_set_qualifier(
                &fauna_core::path_crypto::set_name_hash("members-site")
            )),
            "the scope names the set by its hash, never the plaintext name"
        );
        assert_eq!(blob.wrapped_keys.len(), 2, "one wrap per generation");
        let mut opened: Vec<(u64, Vec<u8>)> = blob
            .wrapped_keys
            .iter()
            .map(|w| {
                (
                    w.epoch.expect("folder wraps carry the generation version"),
                    unseal_capability(w, &owner, &holder_sk).unwrap(),
                )
            })
            .collect();
        opened.sort_by_key(|(v, _)| *v);
        assert_eq!(opened[0], (1, vec![0xC1u8; 32]));
        assert_eq!(opened[1], (2, vec![0xC2u8; 32]));
    }

    #[test]
    fn mint_folder_grant_without_custody_reports_not_found() {
        let cfg = fauna_core::data::FoldersConfig::default();
        let (_sk, holder_pk) = generate_x25519_keypair();
        let channel = fauna_core::folder_keys::serve_custody_channel_id("ghost");
        let err = mint_folder_grant(
            &cfg,
            &[0x88u8; 32],
            &[0x03u8; 16],
            &holder_pk,
            None,
            GrantWindow(0, DEFAULT_GRANT_WINDOW_SECS),
            "ghost",
            &channel,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            MintGrantError::DerivePayload(DerivePayloadError::FolderCustodyNotFound(_))
        ));
    }

    // ── bounded mail grants (content-sealing epochs, B2) ──

    use fauna_mls::wrapped_blob::{
        MAIL_SEALING_EPOCH_SECS, derive_recipient_mail_epoch_capability_secret,
        mail_sealing_epoch_of,
    };

    #[test]
    fn bounded_mail_mint_wraps_one_key_per_window_epoch_and_never_the_standing_secret() {
        let mut mail = MailConfig::default();
        let msek = [0x42u8; 32];
        mail.msek = Some(msek.into());
        let owner = [0x0Au8; 32];
        let grant_id = [0x0Bu8; 16];
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        // A window spanning exactly three weekly epochs.
        let e0 = 3000u64;
        let window = GrantWindow(
            e0 * MAIL_SEALING_EPOCH_SECS + 5,
            (e0 + 2) * MAIL_SEALING_EPOCH_SECS + 5,
        );
        let blob = mint_bounded_mail_grant(
            &mail, &owner, &grant_id, &holder_pk, None, window, /* label-write */ true,
        )
        .unwrap();

        // Scope declares mail read + the keyless label-write.
        assert_eq!(blob.scope.len(), 2);
        assert_eq!(blob.scope[0].kind.as_deref(), Some(ScopeTuple::KIND_MAIL));
        assert_eq!(blob.scope[1].class, ScopeTuple::CLASS_CONTENT_LABEL_WRITE);

        // One wrap per epoch, all Some(e), no standing (None) wrap anywhere.
        assert_eq!(blob.wrapped_keys.len(), 3);
        let epochs: Vec<u64> = blob.wrapped_keys.iter().map(|w| w.epoch.unwrap()).collect();
        assert_eq!(epochs, vec![e0, e0 + 1, e0 + 2]);

        // Each wrap opens to that epoch's capability payload — and the
        // standing master-key payload appears nowhere.
        let standing = derive_recipient_mail_capability_secret(&msek);
        for w in &blob.wrapped_keys {
            let opened = unseal_capability(w, &owner, &holder_sk).unwrap();
            let expected = derive_recipient_mail_epoch_capability_secret(&msek, w.epoch.unwrap());
            assert_eq!(opened, expected);
            assert_ne!(opened, standing);
        }
    }

    /// The per-labeler grant: the same epoch wraps as the
    /// composed mint, but every tuple and wrap names the labeler's factor,
    /// and a wrap presented under any other factor — the composed role's
    /// `None` included — fails to open.
    #[test]
    fn per_labeler_mint_confines_every_tuple_and_wrap_to_the_labeler() {
        let mut mail = MailConfig::default();
        let msek = [0x42u8; 32];
        mail.msek = Some(msek.into());
        let owner = [0x0Au8; 32];
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let e0 = 3000u64;
        let window = GrantWindow(
            e0 * MAIL_SEALING_EPOCH_SECS + 5,
            (e0 + 1) * MAIL_SEALING_EPOCH_SECS + 5,
        );
        let labeler = fauna_core::identity::ActorId([0xCDu8; 32]);
        let factor = fauna_core::scoring::labeler_factor(&labeler);
        let blob = mint_bounded_mail_labeler_grant(
            &mail,
            &owner,
            &[0x0Cu8; 16],
            &holder_pk,
            None,
            window,
            &labeler,
        )
        .unwrap();

        assert_eq!(
            blob.scope.len(),
            2,
            "mail read + label-write, both confined"
        );
        assert!(
            blob.scope
                .iter()
                .all(|t| t.factor.as_deref() == Some(factor.as_str())),
            "every declared tuple names the labeler's factor"
        );
        assert_eq!(blob.wrapped_keys.len(), 2);
        for w in &blob.wrapped_keys {
            assert_eq!(w.scope.factor.as_deref(), Some(factor.as_str()));
            assert_eq!(
                unseal_capability(w, &owner, &holder_sk).unwrap(),
                derive_recipient_mail_epoch_capability_secret(&msek, w.epoch.unwrap()),
                "the per-labeler wrap carries the ordinary epoch secret"
            );
            let mut widened = w.clone();
            widened.scope.factor = None;
            assert!(
                unseal_capability(&widened, &owner, &holder_sk).is_err(),
                "re-labelled as the composed role, the wrap stays sealed"
            );
        }
    }

    /// A retained prior MSEK with **no recorded retirement instant** is not read
    /// as a pre-amendment "legacy retention" any more — that pre-sweep reading
    /// was retired by the compat-remnant sweep (program 4,
    /// `version-compatibility.md` § Dimension 2) — but as an inconsistent
    /// config: its seal interval is unknowable, so it contributes nothing and
    /// every window epoch carries the current generation's secret alone.
    #[test]
    fn an_unrecorded_prior_retention_contributes_no_epoch_secret() {
        let mut mail = MailConfig::default();
        let msek = [0x42u8; 32];
        mail.msek = Some(msek.into());
        mail.prior_mseks = vec![[0x41u8; 32].into()];
        let owner = [0x0Au8; 32];
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let e0 = 3000u64;
        let window = GrantWindow(
            e0 * MAIL_SEALING_EPOCH_SECS + 5,
            (e0 + 2) * MAIL_SEALING_EPOCH_SECS + 5,
        );
        let blob = mint_bounded_mail_grant(
            &mail,
            &owner,
            &[0x0Bu8; 16],
            &holder_pk,
            None,
            window,
            false,
        )
        .unwrap();
        assert_eq!(blob.wrapped_keys.len(), 3);
        for w in &blob.wrapped_keys {
            assert_eq!(
                unseal_capability(w, &owner, &holder_sk).unwrap(),
                derive_recipient_mail_epoch_capability_secret(&msek, w.epoch.unwrap()),
                "only the current generation's secret — the unrecorded prior adds no chunk"
            );
        }
    }

    #[test]
    fn a_post_rotation_bounded_mint_opens_pre_rotation_in_window_content() {
        let mut mail = MailConfig::default();
        // The 2026-07-19 rotation-heal amendment's headline property: the
        // owner hard-revoked (rotated) their MSEK inside epoch 1002 — with
        // the retirement instant recorded — and then mints a bounded grant
        // whose window reaches back across the rotation. The holder must
        // open (a) pre-rotation content sealed under the RETIRED
        // generation's epoch keys, (b) both generations' slices of the
        // boundary epoch, and (c) post-rotation content — while (d) content
        // sealed past the window stays dark even holding every wrapped key.
        use fauna_core::data::PriorMsekRetirement;
        use fauna_mls::wrapped_blob::{
            derive_mail_epoch_root, derive_recipient_epoch_hpke_keypair_from_root,
            seal_to_recipient, unseal_mail_record_with_derived_key,
        };

        let instant_in = |e: u64| e * MAIL_SEALING_EPOCH_SECS + 7;
        let old_msek = [0xA1u8; 32];
        let new_msek = [0xA2u8; 32];
        mail.msek = Some(new_msek.into());
        mail.prior_mseks = vec![old_msek.into()];
        mail.prior_msek_retirements = vec![PriorMsekRetirement {
            msek: old_msek.into(),
            retired_at_unix: instant_in(1002),
        }];

        let owner = [0x0Cu8; 32];
        let (holder_sk, holder_pk) = generate_x25519_keypair();
        let window = GrantWindow(instant_in(1000), instant_in(1004));
        let blob = mint_bounded_mail_grant(
            &mail,
            &owner,
            &[0x0Du8; 16],
            &holder_pk,
            None,
            window,
            false,
        )
        .unwrap();

        // One wrap per window epoch — the single-slot invariant holds even
        // with two generations in play.
        let epochs: Vec<u64> = blob.wrapped_keys.iter().map(|w| w.epoch.unwrap()).collect();
        assert_eq!(epochs, vec![1000, 1001, 1002, 1003, 1004]);

        let payload_for = |e: u64| -> Vec<u8> {
            let w = blob
                .wrapped_keys
                .iter()
                .find(|w| w.epoch == Some(e))
                .unwrap();
            unseal_capability(w, &owner, &holder_sk).unwrap()
        };
        let old_root = derive_mail_epoch_root(&old_msek);
        let new_root = derive_mail_epoch_root(&new_msek);
        let seal_under = |root: &[u8; 32], e: u64, body: &[u8]| {
            let (_sk, pk) = derive_recipient_epoch_hpke_keypair_from_root(root, e);
            seal_to_recipient(body, &pk).unwrap()
        };

        // (a) Pre-rotation epoch, old-generation-sealed → opens.
        let env = seal_under(&old_root, 1000, b"pre-rotation mail");
        assert_eq!(
            unseal_mail_record_with_derived_key(&env, &payload_for(1000)).unwrap(),
            b"pre-rotation mail"
        );

        // (b) The boundary epoch: BOTH generations' slices open from the one
        // concatenated payload.
        let env_old = seal_under(&old_root, 1002, b"boundary, sealed before the rotation");
        let env_new = seal_under(&new_root, 1002, b"boundary, sealed after the rotation");
        let boundary_payload = payload_for(1002);
        assert_eq!(
            unseal_mail_record_with_derived_key(&env_old, &boundary_payload).unwrap(),
            b"boundary, sealed before the rotation"
        );
        assert_eq!(
            unseal_mail_record_with_derived_key(&env_new, &boundary_payload).unwrap(),
            b"boundary, sealed after the rotation"
        );

        // (c) Post-rotation epoch, current-generation-sealed → opens.
        let env = seal_under(&new_root, 1004, b"post-rotation mail");
        assert_eq!(
            unseal_mail_record_with_derived_key(&env, &payload_for(1004)).unwrap(),
            b"post-rotation mail"
        );

        // (d) The crypto bound is unregressed: content sealed one epoch past
        // the window fails every payload the holder ever received — under
        // BOTH generations' keys.
        for root in [&old_root, &new_root] {
            let env = seal_under(root, 1005, b"sealed past the window");
            for e in 1000..=1004 {
                assert!(
                    unseal_mail_record_with_derived_key(&env, &payload_for(e)).is_err(),
                    "epoch-1005 content must stay dark to a [1000,1004] holder"
                );
            }
        }
    }

    #[test]
    fn bounded_mail_mint_without_msek_reports_mail_not_enabled() {
        let mail = MailConfig::default();
        let err = mint_bounded_mail_grant(
            &mail,
            &[0u8; 32],
            &[0u8; 16],
            &[9u8; 32],
            None,
            GrantWindow(0, 1),
            false,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            MintGrantError::DerivePayload(DerivePayloadError::MailNotEnabled)
        ));
    }

    #[test]
    fn renewal_keys_cover_exactly_the_extension_epochs() {
        let mut mail = MailConfig::default();
        let msek = [0x42u8; 32];
        mail.msek = Some(msek.into());
        let owner = [0x0Au8; 32];
        let (holder_sk, holder_pk) = generate_x25519_keypair();

        let old_end = 3000 * MAIL_SEALING_EPOCH_SECS + 100;
        // Extension inside the same epoch → pure window bump, no new keys.
        assert!(
            bounded_mail_renewal_keys(&mail, &owner, &holder_pk, None, old_end, old_end + 10, None)
                .unwrap()
                .is_empty()
        );

        // Extension into two later epochs → exactly those epochs' wraps.
        let new_end = old_end + 2 * MAIL_SEALING_EPOCH_SECS;
        let keys =
            bounded_mail_renewal_keys(&mail, &owner, &holder_pk, None, old_end, new_end, None)
                .unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].epoch, Some(3001));
        assert_eq!(keys[1].epoch, Some(3002));
        assert_eq!(mail_sealing_epoch_of(new_end), 3002);
        for k in &keys {
            let opened = unseal_capability(k, &owner, &holder_sk).unwrap();
            assert_eq!(
                opened,
                derive_recipient_mail_epoch_capability_secret(&msek, k.epoch.unwrap())
            );
        }
    }

    /// A per-labeler grant's renewal and rotation-heal wraps carry its factor,
    /// exactly as its mint did — a factor-less appended wrap would widen the
    /// grant to the composed read, which the holder's factor-selecting fetch
    /// would then honour.
    #[test]
    fn renewal_and_heal_wraps_keep_the_labeler_factor() {
        let mail = MailConfig {
            msek: Some([0x42u8; 32].into()),
            ..MailConfig::default()
        };
        let owner = [0x0Au8; 32];
        let (_holder_sk, holder_pk) = generate_x25519_keypair();
        let factor =
            fauna_core::scoring::labeler_factor(&fauna_core::identity::ActorId([0xCDu8; 32]));

        let old_end = 3000 * MAIL_SEALING_EPOCH_SECS + 100;
        let renewal = bounded_mail_renewal_keys(
            &mail,
            &owner,
            &holder_pk,
            None,
            old_end,
            old_end + MAIL_SEALING_EPOCH_SECS,
            Some(&factor),
        )
        .unwrap();
        assert_eq!(renewal.len(), 1);
        assert_eq!(renewal[0].scope.factor.as_deref(), Some(factor.as_str()));

        let window = GrantWindow(old_end, old_end + MAIL_SEALING_EPOCH_SECS);
        let heal = bounded_mail_rotation_heal_keys(
            &mail,
            &owner,
            &holder_pk,
            None,
            &window,
            Some(&factor),
        )
        .unwrap();
        assert!(!heal.is_empty());
        assert!(
            heal.iter()
                .all(|w| w.scope.factor.as_deref() == Some(factor.as_str())),
            "every healed wrap stays confined to the labeler"
        );
        // …and the composed role's wraps stay factor-less.
        let composed =
            bounded_mail_rotation_heal_keys(&mail, &owner, &holder_pk, None, &window, None)
                .unwrap();
        assert!(composed.iter().all(|w| w.scope.factor.is_none()));
    }

    // ── the principal folder grant's id and generation walk ──

    const SECRET: [u8; 32] = [0xA0; 32];
    const HOLDER: [u8; 32] = [0xB0; 32];

    fn principal_id(generation: u32) -> [u8; 16] {
        folder_principal_grant_id(&SECRET, &HOLDER, "photos", generation)
    }

    fn mint(generation: u32, at: u64) -> fauna_core::grant_event::GrantEvent {
        grant_log::build_mint_event(principal_id(generation), HOLDER, vec![], at, at + 10, at)
    }

    fn revoke(generation: u32, at: u64) -> fauna_core::grant_event::GrantEvent {
        grant_log::build_revoke_event(principal_id(generation), HOLDER, at)
    }

    fn walk(events: &[fauna_core::grant_event::GrantEvent]) -> (u32, Option<Vec<u8>>) {
        let g = folder_principal_generation(events, &SECRET, &HOLDER, "photos");
        (g.generation, g.live.map(|c| c.grant_id))
    }

    /// The same (holder, set, generation) derives the same id on every call;
    /// any one input changing moves it — the paywall id's included.
    #[test]
    fn the_principal_folder_id_is_derived_per_holder_set_and_generation() {
        assert_eq!(principal_id(0), principal_id(0));
        assert_ne!(principal_id(0), principal_id(1));
        assert_ne!(
            principal_id(0),
            folder_principal_grant_id(&SECRET, &[0xB1; 32], "photos", 0)
        );
        assert_ne!(
            principal_id(0),
            folder_principal_grant_id(&SECRET, &HOLDER, "docs", 0)
        );
        assert_ne!(
            principal_id(0),
            folder_principal_grant_id(&[0xA1; 32], &HOLDER, "photos", 0)
        );
        assert_ne!(
            principal_id(0),
            folder_paywall_grant_id(&SECRET, "photos", 0)
        );
    }

    /// The paywall id moves with its generation, set and secret, and walks
    /// the same way the principal id does: `Mint(0)` live, `Revoke(0)` spent
    /// (generation 1 fresh, its id named), `Mint(1)` live. The resolver maps
    /// both generations back to the set and names nothing of a set not
    /// listed.
    #[test]
    fn the_paywall_grant_id_takes_the_generation_and_shares_the_walk() {
        let paywall = |set: &str, generation| folder_paywall_grant_id(&SECRET, set, generation);
        assert_eq!(paywall("photos", 0), paywall("photos", 0));
        assert_ne!(paywall("photos", 0), paywall("photos", 1));
        assert_ne!(paywall("photos", 0), paywall("docs", 0));
        assert_ne!(
            paywall("photos", 0),
            folder_paywall_grant_id(&[0xA1; 32], "photos", 0)
        );
        let mint = |generation, at| {
            grant_log::build_mint_event(
                paywall("photos", generation),
                HOLDER,
                vec![],
                at,
                at + 10,
                at,
            )
        };
        let revoke = |generation, at| {
            grant_log::build_revoke_event(paywall("photos", generation), HOLDER, at)
        };
        let walk = |events: &[fauna_core::grant_event::GrantEvent]| {
            let g = folder_paywall_generation(events, &SECRET, "photos");
            (g.generation, g.grant_id, g.live.map(|c| c.grant_id))
        };
        assert_eq!(walk(&[]), (0, paywall("photos", 0), None));
        let live0 = Some(paywall("photos", 0).to_vec());
        assert_eq!(walk(&[mint(0, 100)]), (0, paywall("photos", 0), live0));
        let spent = [mint(0, 100), revoke(0, 200)];
        assert_eq!(walk(&spent), (1, paywall("photos", 1), None));
        let again = [mint(0, 100), revoke(0, 200), mint(1, 300)];
        let live1 = Some(paywall("photos", 1).to_vec());
        assert_eq!(walk(&again), (1, paywall("photos", 1), live1));
        // A principal grant over the same set never moves the paywall walk.
        assert_eq!(walk(&[self::mint(0, 100)]), (0, paywall("photos", 0), None));

        let named = folder_paywall_set_names(&again, &SECRET, &["photos".to_owned()]);
        assert_eq!(
            named.get(&paywall("photos", 0)).map(String::as_str),
            Some("photos")
        );
        assert_eq!(
            named.get(&paywall("photos", 1)).map(String::as_str),
            Some("photos")
        );
        assert_eq!(named.len(), 2);
        assert!(folder_paywall_set_names(&again, &SECRET, &["docs".to_owned()]).is_empty());
    }

    /// The walk: an empty log is generation 0, fresh; a live `Mint(0)` is
    /// generation 0, live; `Revoke(0)` spends it (generation 1, fresh); a
    /// `Mint(1)` after it is generation 1, live.
    #[test]
    fn the_generation_walk_skips_spent_ids_and_stops_at_live_or_fresh() {
        assert_eq!(walk(&[]), (0, None));
        assert_eq!(walk(&[mint(0, 100)]), (0, Some(principal_id(0).to_vec())));
        assert_eq!(walk(&[mint(0, 100), revoke(0, 200)]), (1, None));
        assert_eq!(
            walk(&[mint(0, 100), revoke(0, 200), mint(1, 300)]),
            (1, Some(principal_id(1).to_vec()))
        );
        // A revoke merged in before its mint spends the generation alike.
        assert_eq!(walk(&[revoke(0, 200)]), (1, None));
        // Another holder's or set's events never move this walk.
        let other = grant_log::build_mint_event(
            folder_principal_grant_id(&SECRET, &HOLDER, "docs", 0),
            HOLDER,
            vec![],
            1,
            2,
            1,
        );
        assert_eq!(walk(&[other]), (0, None));
    }

    /// The folder resolver: every generation the log holds for (holder, set)
    /// maps back to the set — a spent one too, so a History-lens `Revoke`
    /// names its folder — while an id of another holder, or of a set not
    /// listed (a deleted one), maps to nothing.
    #[test]
    fn the_principal_folder_resolver_maps_every_held_generation_to_its_set() {
        let other_set = |set: &str| {
            grant_log::build_mint_event(
                folder_principal_grant_id(&SECRET, &HOLDER, set, 0),
                HOLDER,
                vec![],
                1,
                2,
                1,
            )
        };
        let events = [
            mint(0, 100),
            revoke(0, 200),
            mint(1, 300),
            other_set("docs"),
            other_set("gone"),
        ];
        let sets = ["photos".to_owned(), "docs".to_owned(), "empty".to_owned()];
        let named = folder_principal_set_names(&events, &SECRET, &HOLDER, &sets);
        let name = |id: [u8; 16]| named.get(&id).map(String::as_str);
        assert_eq!(name(principal_id(0)), Some("photos"));
        assert_eq!(name(principal_id(1)), Some("photos"));
        assert_eq!(
            name(folder_principal_grant_id(&SECRET, &HOLDER, "docs", 0)),
            Some("docs")
        );
        // The deleted set "gone" is not listed; "empty" holds no grant.
        assert_eq!(named.len(), 3);
        // Another holder's view of the same log names nothing.
        assert!(folder_principal_set_names(&events, &SECRET, &[0xB1; 32], &sets).is_empty());
    }

    /// Rule (4)'s finder names each holder's LIVE generation over the set —
    /// generation 1 after a revoke and a re-mint — and nothing else: not a
    /// spent generation, not another set's twin, not a non-twin grant.
    #[test]
    fn the_live_principal_grants_over_a_set_are_each_holders_live_generation() {
        let other_holder = [0xB1; 32];
        let other_set = grant_log::build_mint_event(
            folder_principal_grant_id(&SECRET, &other_holder, "docs", 0),
            other_holder,
            vec![],
            1,
            2,
            1,
        );
        let records = grant_log::build_mint_event([0x77; 16], HOLDER, vec![], 1, 2, 1);
        let events = [
            mint(0, 100),
            revoke(0, 200),
            mint(1, 300),
            other_set,
            records,
        ];
        let live: Vec<Vec<u8>> = live_folder_principal_grants(&events, &SECRET, "photos")
            .into_iter()
            .map(|g| g.grant_id)
            .collect();
        assert_eq!(live, vec![principal_id(1).to_vec()]);
        assert!(live_folder_principal_grants(&events[..2], &SECRET, "photos").is_empty());
    }
}
