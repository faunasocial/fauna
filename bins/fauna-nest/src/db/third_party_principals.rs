//! The third-party principal roster (`docs/goal/architecture/third-party.md`
//! § The principal model, TP1): one `third_party_principals` row per approved
//! Client ID Metadata Document per account.
//!
//! **Minted by the consent act, inside the grant's own transaction.** The row
//! is found-or-created by [`upsert_principal_in_tx`], which
//! [`CacheDb::record_atproto_oauth_grant`] calls in the transaction that writes
//! the grant row and its session family. So a grant never lands without its
//! principal, and a principal never lands for a ceremony whose grant rolled
//! back: the connected-apps row's two halves (the roster row here, the grant
//! row one table over) cannot disagree about whether a connection exists.
//!
//! **Revocation is one verb** (rule 4) — [`CacheDb::revoke_third_party_principal`]
//! ends, in one transaction, every grant family the principal accrued (each
//! ceremony mints its own, so there may be several), every capability grant
//! the account minted to the principal's key, and the row itself.
//!
//! Plaintext-floor routing metadata — a client identity and a public key, the
//! same class as `bridge_service_users` — and never key material.

use anyhow::{Context, Result, anyhow};
use rusqlite::OptionalExtension;

use super::atproto_pds::{revoke_grant_row, revoke_session_row};
use super::{CacheDb, now_epoch_millis};
use fauna_protocol::kind_manifest::ServiceAuthEntry;

/// Length of a nest-minted `principal_id`.
pub const PRINCIPAL_ID_LEN: usize = 16;

/// The reserved owner of a hosted plugin's INSTALL row (`third-party.md`
/// § The principal model → *Hosted principals*): the all-zero actor id, the
/// placeholder every nest-side-not-an-account slot already uses (an anonymous
/// or principal session's actor slot). No account has it — an actor id is an
/// Ed25519 public key, and the zero point is nobody's — so the install row
/// collides with no user's roster, and a user's `fauna.principals.list` never
/// shows it. The row is reached by the admin's `fauna.plugins.*` kinds.
pub const NEST_OWNER_ACTOR: [u8; 32] = [0u8; 32];

/// How the principal's code runs (`third-party.md` § Execution forms): the
/// two forms a consent mints, and the hosted form the admin's install
/// approval mints (`container` waits for the catalog + supervisor slice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionForm {
    /// A server of its own — derived from a CONFIDENTIAL client
    /// (`private_key_jwt`): only a party that can keep a private key off the
    /// user's device can authenticate that way.
    Remote,
    /// An app on the user's own device — a PUBLIC client.
    Device,
    /// A WASM component the nest hosts and supervises
    /// (`libs/fauna-plugin-host`). Minted by the install leg, never by a
    /// consent: a user's consent to an installed plugin's document mints a
    /// BINDING row that copies this form from the install row.
    Wasm,
}

impl ExecutionForm {
    /// The derivation rule 3 asks for: from what the consent established about
    /// the client (its authentication method, read off its own document),
    /// never from anything the process claims about itself.
    pub fn for_client(confidential: bool) -> Self {
        if confidential {
            Self::Remote
        } else {
            Self::Device
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Device => "device",
            Self::Wasm => "wasm",
        }
    }

    /// The column's spelling back to the form; `None` for a spelling this
    /// build does not know (a `container` row from a later build).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "remote" => Some(Self::Remote),
            "device" => Some(Self::Device),
            "wasm" => Some(Self::Wasm),
            _ => None,
        }
    }

    /// Is this a form the nest runs itself?
    pub fn is_hosted(self) -> bool {
        matches!(self, Self::Wasm)
    }
}

/// What the install leg hands the mint (`third-party.md` § The runner
/// contract → *The install-approval leg*): the document's identity, the
/// members the verified manifest carried, and the holder key the host minted
/// into the plugin's state scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedPluginMint {
    /// The document URL.
    pub client_id: String,
    /// The resolved display name the install card showed.
    pub label: Option<String>,
    /// The host-minted holder key's PUBLIC half.
    pub holder_x25519: [u8; 32],
    /// The verified manifest's `publisher.key`, raw.
    pub publisher_key: [u8; 32],
    /// The verified manifest's `kinds`, each the full `ext.*` string.
    pub declared_kinds: Vec<String>,
    /// The scopes the document declares and the admin approved the plugin
    /// may ask each user for — the ceiling of every binding row's
    /// `granted_scopes`; the install itself grants nothing (rule 1).
    pub requested_scopes: String,
    /// The `execution.digest` the fetch verified (`sha256:…`).
    pub module_digest: String,
    /// The `execution.hosts` the plugin may dial.
    pub hosts: Vec<String>,
    /// The `ingress` paths the nest terminates for it, verbatim JSON.
    pub ingress: serde_json::Value,
    /// The `settings_schema`, verbatim JSON, when declared.
    pub settings_schema: Option<serde_json::Value>,
    /// The admin who approved the install.
    pub installed_by: [u8; 32],
}

/// One installed plugin, as the admin's `fauna.plugins.list` and the runner
/// read it: the install row's principal fields joined with the hosted half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedPluginRow {
    pub principal_id: Vec<u8>,
    pub client_id: String,
    pub label: Option<String>,
    pub holder_x25519: Vec<u8>,
    pub publisher_key: Vec<u8>,
    pub declared_kinds: Vec<String>,
    pub requested_scopes: String,
    pub module_digest: String,
    pub hosts: Vec<String>,
    pub ingress: serde_json::Value,
    pub settings_schema: Option<serde_json::Value>,
    pub installed_by: Vec<u8>,
    pub installed_at: i64,
    /// Accounts whose own consent bound them to this plugin.
    pub bound_accounts: Vec<[u8; 32]>,
}

/// Why an install was refused at the mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InstallRefused {
    /// The document is already installed; update is a different verb.
    #[error("this document is already installed on this nest")]
    AlreadyInstalled,
}

/// What one uninstall ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PluginUninstalled {
    /// Binding rows ended, each through the one revoke cascade.
    pub bindings_ended: u32,
    /// Grant families those bindings held.
    pub grants_ended: u32,
    /// Capability grants those accounts had minted to the plugin's key.
    pub capability_grants_ended: u32,
    /// State-scope entries deleted.
    pub state_entries_deleted: u32,
}

/// The two public keys a client may attest at the start of its ceremony
/// (`third-party.md` § The principal model, rule 2;
/// `third-party-kinds.md` § Principal write authority) — carried together from
/// the start parameters to the roster row, so no door can carry one and drop
/// the other. Each is bound, not proven: a client that attests a key it does
/// not hold can open nothing wrapped to it and sign nothing under it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttestedKeys {
    /// The X25519 key capability grants are wrapped to (`fauna_holder_x25519`).
    pub holder_x25519: Option<[u8; 32]>,
    /// The Ed25519 key the principal signs its `ext.*` rows with
    /// (`fauna_writer_ed25519`) — the writer a `content.write` grant names.
    pub writer_ed25519: Option<[u8; 32]>,
}

/// What the document's verified kind manifest said, as the roster row keeps
/// it (`third-party-kinds.md` § The manifest): the publisher key the
/// consenting ceremony saw, and the kinds it declared — the index the nest's
/// record doors serve `ext:<kind>` scopes by, never an engine's authority.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrincipalManifest {
    pub publisher_key: [u8; 32],
    pub declared_kinds: Vec<String>,
    /// The manifest's `service_auth` entries (`third-party.md` § The
    /// manifest) — the roster row's `declared_service_auth`, the set the
    /// oracle's `atproto.service_auth` class is bounded by.
    pub declared_service_auth: Vec<ServiceAuthEntry>,
    /// The validated `bridge` block (`third-party.md` § The manifest → *The
    /// `bridge` block`), `None` for a document that is not a conversation
    /// bridge — the roster row's `declared_bridge`.
    pub bridge: Option<fauna_protocol::kind_manifest::BridgeBlock>,
    /// The validated `events_uri` (`transport.md` § Push events →
    /// *Third-party event doors*, the webhook), `None` for a document that
    /// declares none — the roster row's `events_uri`.
    pub events_uri: Option<String>,
}

impl PrincipalManifest {
    /// The roster's view of a manifest [`fauna_protocol::kind_manifest::verify_manifest`]
    /// accepted.
    pub fn of(verified: &fauna_protocol::kind_manifest::VerifiedManifest) -> Self {
        Self {
            publisher_key: verified.publisher_key,
            declared_kinds: verified.kinds.iter().map(|k| k.kind.to_string()).collect(),
            declared_service_auth: verified.service_auth.clone(),
            bridge: verified.bridge.clone(),
            events_uri: verified.events_uri.clone(),
        }
    }
}

/// What the ceremony learned about the principal beyond the grant row's own
/// fields — passed through [`CacheDb::record_atproto_oauth_grant`] to the
/// find-or-create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalAttestation {
    /// The keys the client attested at its start, if it presented any.
    pub keys: AttestedKeys,
    pub execution_form: ExecutionForm,
    /// The document's kind manifest, verified at resolution — `None` for a
    /// document with no `fauna` member.
    pub manifest: Option<PrincipalManifest>,
}

/// A public client that attested no key — the shape every pre-principal test
/// fixture recorded a grant as.
#[cfg(test)]
pub(crate) const UNATTESTED_DEVICE: PrincipalAttestation = PrincipalAttestation {
    keys: AttestedKeys {
        holder_x25519: None,
        writer_ed25519: None,
    },
    execution_form: ExecutionForm::Device,
    manifest: None,
};

/// Why a consent's attested key was refused. Typed so `/oauth/token` answers
/// it as the client's error rather than as a server fault — nothing is broken;
/// the client attested a key it may not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HolderKeyRefused {
    /// Another of this account's principals already holds the key. Revoke ends
    /// the capability grants held by a principal's key, so two rows sharing one
    /// would let revoking one silently end the other's.
    #[error("the attested holder key is already held by another connected app")]
    HeldByAnotherPrincipal,
    /// An enrolled bridge or content processor's key. A client that attested
    /// it would, on revoke, end the account's grants to that bridge — the web
    /// paywall's holder, the mail bridge's — which the user never connected
    /// through this app.
    #[error("the attested holder key belongs to a bridge enrolled on this nest")]
    HeldByABridge,
    /// Another of this account's principals already writes under the key. A
    /// `content.write` grant names its writer, so two principals sharing one
    /// would each be authorized as the other.
    #[error("the attested writer key is already held by another connected app")]
    WriterHeldByAnotherPrincipal,
    /// A key this nest has already seen sign one of the account's own rows (a
    /// device's, the account's own) or an enrolled bridge's. A principal
    /// attesting it would be granted write authority as that writer.
    #[error("the attested writer key already writes for this account or a bridge on this nest")]
    WriterKnown,
    /// Another of this account's principals already declares the document's
    /// `bridge.id` — the id is unique per account roster at consent
    /// (`third-party.md` § The manifest → *The `bridge` block*), because a
    /// bridged room, a deposit and the family gate's verdict row are all keyed
    /// by it.
    #[error("another connected app already serves this account as the same bridge")]
    BridgeIdHeldByAnotherPrincipal,
}

/// One roster row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalRow {
    pub principal_id: Vec<u8>,
    pub client_id: String,
    pub holder_x25519: Option<Vec<u8>>,
    pub execution_form: String,
    pub declared_kinds: Vec<String>,
    pub granted_scopes: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub label: Option<String>,
    pub publisher_key: Option<Vec<u8>>,
    /// The Ed25519 writer key the principal attested, if it attested one.
    pub writer_ed25519: Option<Vec<u8>>,
    /// The consented document's `bridge` block, if it declares one.
    pub declared_bridge: Option<fauna_protocol::kind_manifest::BridgeBlock>,
    /// The consented document's `service_auth` entries, `[]` for none.
    pub declared_service_auth: Vec<ServiceAuthEntry>,
    /// Live grant families (the `list_atproto_oauth_grants` liveness
    /// predicate: not revoked, not past `expires_at`).
    pub live_grants: u32,
}

/// One principal's webhook, as [`CacheDb::list_event_webhooks`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventWebhook {
    pub principal_id: Vec<u8>,
    pub client_id: String,
    /// The row's granted scopes, space-separated as recorded.
    pub granted_scopes: String,
    /// The validated `events_uri` the consented document declared.
    pub events_uri: String,
}

/// What one revoke ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrincipalRevoked {
    pub grants_ended: u32,
    pub capability_grants_ended: u32,
    /// The bridged rooms whose undrained outbox the revoke deleted, each
    /// owed a `conversation_changed` nudge after the commit
    /// (`bridged_conversations::end_bridged_outbox_in_tx`).
    pub outbox_rooms: Vec<Vec<u8>>,
}

/// Find-or-create the principal for `(actor, client_id)`, inside the caller's
/// transaction — the consent's mint.
///
/// - **No row** → a new one, `label` = the resolved name the card showed.
/// - **A row** → the same principal: `last_used_at` and `granted_scopes` take
///   this ceremony's values (the latest consent is what the user most recently
///   agreed to), `label` stays as minted.
///
/// **Each attested key follows the latest attestation.** A ceremony that
/// presents a key sets it; one that presents none leaves the row's key as it
/// was (a standard client never presents one, and silence is not a
/// retraction). A ceremony that presents a DIFFERENT holder key replaces it,
/// and the capability grants the account minted to the old key end in the
/// same act: they are reach held by a key no roster row names any more, which
/// the user could neither see nor revoke. A different WRITER key replaces it
/// the same way and ends the grants that carry a `content.write` tuple — each
/// names the old writer in its factor, an authority no roster row would name
/// (`third-party-kinds.md` § Principal write authority).
///
/// **The manifest follows the latest document.** `publisher_key`,
/// `declared_kinds` and `declared_service_auth` are what the consented document's verified `fauna` member
/// said, cleared when it carries none: the row is the index of what the user
/// last approved, and a scope an earlier manifest opened stays served by the
/// rows it already holds (`third-party-kinds.md` § The `ext` sub-scope).
pub(crate) fn upsert_principal_in_tx(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    client_id: &str,
    label: Option<&str>,
    granted_scopes: &str,
    attestation: &PrincipalAttestation,
    now: i64,
) -> Result<()> {
    let existing: Option<(Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>, Option<String>)> = tx
        .query_row(
            "SELECT principal_id, holder_x25519, writer_ed25519,
                    json_extract(declared_bridge, '$.id')
               FROM third_party_principals
              WHERE actor_id = ?1 AND client_id = ?2",
            rusqlite::params![&actor_id[..], client_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .context("read third-party principal")?;

    // A consent to an INSTALLED plugin's document mints a binding row: the
    // form is the install row's, and the key — which no hosted plugin can
    // attest at a start it never makes — is the host-minted one the install
    // row holds, unless the ceremony presented one (a developer's own test
    // client may). The consent still decides the scopes and the grant.
    let hosted: Option<(String, Option<Vec<u8>>)> = tx
        .query_row(
            "SELECT p.execution_form, p.holder_x25519
               FROM hosted_plugins h JOIN third_party_principals p
                 ON p.actor_id = ?1 AND p.principal_id = h.principal_id
              WHERE h.client_id = ?2",
            rusqlite::params![&NEST_OWNER_ACTOR[..], client_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("read installed plugin for consent")?;
    let (execution_form, install_key) = match &hosted {
        Some((form, key)) => (
            form.as_str(),
            key.as_deref().and_then(|k| <[u8; 32]>::try_from(k).ok()),
        ),
        None => (attestation.execution_form.as_str(), None),
    };
    let holder = attestation.keys.holder_x25519.or(install_key);
    let writer = attestation.keys.writer_ed25519;
    let changes = |old: Option<&Option<Vec<u8>>>, new: Option<[u8; 32]>| match (old, new) {
        (Some(old), Some(new)) => old.as_deref() != Some(&new[..]),
        (None, Some(_)) => true,
        (_, None) => false,
    };
    let holder_changes = changes(existing.as_ref().map(|e| &e.1), holder);
    let writer_changes = changes(existing.as_ref().map(|e| &e.2), writer);
    if holder_changes && let Some(key) = holder {
        refuse_foreign_holder(tx, actor_id, client_id, &key)?;
    }
    if writer_changes && let Some(key) = writer {
        refuse_foreign_writer(tx, actor_id, client_id, &key)?;
    }
    let bridge = attestation
        .manifest
        .as_ref()
        .and_then(|m| m.bridge.as_ref());
    if let Some(bridge) = bridge {
        refuse_foreign_bridge_id(tx, actor_id, client_id, &bridge.id)?;
    }
    let declared_bridge = bridge
        .map(serde_json::to_string)
        .transpose()
        .context("encode declared bridge")?;
    let publisher_key = attestation.manifest.as_ref().map(|m| m.publisher_key);
    let declared_kinds = serde_json::to_string(
        attestation
            .manifest
            .as_ref()
            .map_or(&[][..], |m| m.declared_kinds.as_slice()),
    )
    .context("encode declared kinds")?;
    let declared_service_auth = serde_json::to_string(
        attestation
            .manifest
            .as_ref()
            .map_or(&[][..], |m| m.declared_service_auth.as_slice()),
    )
    .context("encode declared service auth")?;
    let events_uri = attestation
        .manifest
        .as_ref()
        .and_then(|m| m.events_uri.as_deref());

    // The bridged rooms follow the bridge id, not the row (`apps/bridges.md`
    // § Phase G → *When the bridge stops serving*): a row that stops serving
    // its id — the block dropped or moved, or the holder key replaced so the
    // queued ciphertext is dead — ends its undrained outbox here, and a row
    // that comes to declare an id adopts the id's rooms below, both in this
    // transaction.
    let served_bridge_id = bridge.map(|b| b.id.as_str());
    let serving_key = holder.or_else(|| {
        existing
            .as_ref()
            .and_then(|e| e.1.as_deref())
            .and_then(|k| <[u8; 32]>::try_from(k).ok())
    });
    if let Some((principal_id, _, _, old_bridge_id)) = &existing
        && old_bridge_id.is_some()
        && (holder_changes || old_bridge_id.as_deref() != served_bridge_id)
    {
        super::bridged_conversations::end_bridged_outbox_in_tx(tx, actor_id, principal_id, now)?;
    }

    match existing {
        None => {
            let mut principal_id = [0u8; PRINCIPAL_ID_LEN];
            getrandom::fill(&mut principal_id).context("principal id entropy")?;
            tx.execute(
                "INSERT INTO third_party_principals
                    (actor_id, principal_id, client_id, holder_x25519, execution_form,
                     declared_kinds, granted_scopes, created_at, last_used_at, label,
                     publisher_key, writer_ed25519, declared_bridge, declared_service_auth,
                     events_uri)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                rusqlite::params![
                    &actor_id[..],
                    &principal_id[..],
                    client_id,
                    holder.as_ref().map(|k| &k[..]),
                    execution_form,
                    declared_kinds,
                    granted_scopes,
                    now,
                    label,
                    publisher_key.as_ref().map(|k| &k[..]),
                    writer.as_ref().map(|k| &k[..]),
                    declared_bridge,
                    declared_service_auth,
                    events_uri,
                ],
            )
            .context("insert third-party principal")?;
            adopt_bridged_rooms(tx, actor_id, &principal_id, bridge, serving_key)?;
        }
        Some((principal_id, old_holder, old_writer, _)) => {
            if holder_changes && let Some(old) = &old_holder {
                end_capability_grants_held_by(tx, actor_id, old)?;
            } else if writer_changes
                && let Some(old_writer) = old_writer
                    .as_deref()
                    .and_then(|w| <[u8; 32]>::try_from(w).ok())
                && let Some(holder) = &old_holder
            {
                end_content_write_grants_held_by(tx, actor_id, holder, &old_writer)?;
            }
            tx.execute(
                "UPDATE third_party_principals
                    SET last_used_at = ?3,
                        granted_scopes = ?4,
                        execution_form = ?5,
                        holder_x25519 = COALESCE(?6, holder_x25519),
                        writer_ed25519 = COALESCE(?7, writer_ed25519),
                        publisher_key = ?8,
                        declared_kinds = ?9,
                        declared_bridge = ?10,
                        declared_service_auth = ?11,
                        events_uri = ?12
                  WHERE actor_id = ?1 AND principal_id = ?2",
                rusqlite::params![
                    &actor_id[..],
                    principal_id,
                    now,
                    granted_scopes,
                    execution_form,
                    holder.as_ref().map(|k| &k[..]),
                    writer.as_ref().map(|k| &k[..]),
                    publisher_key.as_ref().map(|k| &k[..]),
                    declared_kinds,
                    declared_bridge,
                    declared_service_auth,
                    events_uri,
                ],
            )
            .context("update third-party principal")?;
            adopt_bridged_rooms(tx, actor_id, &principal_id, bridge, serving_key)?;
        }
    }
    Ok(())
}

/// The consent's rebind: a row declaring a `bridge` block with a key to seal
/// to adopts every room of `(account, bridge id)` —
/// [`super::bridged_conversations::rebind_bridged_rooms_in_tx`]. A row with no
/// block, or no key, serves nothing and adopts nothing.
fn adopt_bridged_rooms(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    principal_id: &[u8],
    bridge: Option<&fauna_protocol::kind_manifest::BridgeBlock>,
    key: Option<[u8; 32]>,
) -> Result<()> {
    let (Some(bridge), Some(key)) = (bridge, key) else {
        return Ok(());
    };
    let seat = super::bridged_conversations::BridgeSeat {
        principal_id,
        bridge,
        bridge_x25519: &key,
    };
    super::bridged_conversations::rebind_bridged_rooms_in_tx(tx, actor_id, &bridge.id, &seat)?;
    Ok(())
}

/// The `bridge.id` must be this principal's alone on the account's roster.
/// See [`HolderKeyRefused::BridgeIdHeldByAnotherPrincipal`].
fn refuse_foreign_bridge_id(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    client_id: &str,
    bridge_id: &str,
) -> Result<()> {
    let taken: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM third_party_principals
                             WHERE actor_id = ?1 AND client_id != ?2
                               AND json_extract(declared_bridge, '$.id') = ?3)",
            rusqlite::params![&actor_id[..], client_id, bridge_id],
            |r| r.get(0),
        )
        .context("check bridge id collision")?;
    if taken {
        return Err(anyhow!(HolderKeyRefused::BridgeIdHeldByAnotherPrincipal));
    }
    Ok(())
}

/// The key must be this principal's alone: not another of the account's
/// principals', not an enrolled bridge's. See [`HolderKeyRefused`].
fn refuse_foreign_holder(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    client_id: &str,
    key: &[u8; 32],
) -> Result<()> {
    let sibling: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM third_party_principals
                             WHERE actor_id = ?1 AND holder_x25519 = ?2 AND client_id != ?3)",
            rusqlite::params![&actor_id[..], &key[..], client_id],
            |r| r.get(0),
        )
        .context("check principal holder collision")?;
    if sibling {
        return Err(anyhow!(HolderKeyRefused::HeldByAnotherPrincipal));
    }
    let bridge: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM bridge_service_users WHERE x25519_pubkey = ?1)",
            rusqlite::params![&key[..]],
            |r| r.get(0),
        )
        .context("check bridge holder collision")?;
    if bridge {
        return Err(anyhow!(HolderKeyRefused::HeldByABridge));
    }
    Ok(())
}

/// The writer key must be this principal's alone, and not one the nest has
/// already seen write for the account: not another principal's writer, not the
/// account's own identity key, not any `writer_id` on the account's state
/// feeds (cleartext to the nest — every device that ever put a row), not an
/// enrolled bridge's signing key (`third-party-kinds.md` § Principal write
/// authority). See [`HolderKeyRefused`]'s writer variants.
fn refuse_foreign_writer(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    client_id: &str,
    key: &[u8; 32],
) -> Result<()> {
    let sibling: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM third_party_principals
                             WHERE actor_id = ?1 AND writer_ed25519 = ?2 AND client_id != ?3)",
            rusqlite::params![&actor_id[..], &key[..], client_id],
            |r| r.get(0),
        )
        .context("check principal writer collision")?;
    if sibling {
        return Err(anyhow!(HolderKeyRefused::WriterHeldByAnotherPrincipal));
    }
    let known: bool = key == actor_id
        || tx
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sync_changes
                                 WHERE actor_id = ?1 AND origin_writer = ?2)
                     OR EXISTS (SELECT 1 FROM bridge_service_users WHERE ed25519_pubkey = ?2)",
                rusqlite::params![&actor_id[..], &key[..]],
                |r| r.get(0),
            )
            .context("check known writer collision")?;
    if known {
        return Err(anyhow!(HolderKeyRefused::WriterKnown));
    }
    Ok(())
}

/// Delete every capability grant `actor_id` minted to `holder` that carries a
/// `content.write` tuple licensing `old_writer` — what a replaced writer key
/// ends: the tuple's factor names its writer
/// (`fauna_core::grant_event::writer_factor`). Matching the factor, not the
/// class alone, spares the grant the same consent's approve already
/// deposited for the NEW writer, and is the selector the owner's app records
/// its `Revoke`s by (`view_model::grants_ended_by_key_replacement`). The
/// declared scope rides the blob in the clear (the nest decodes the header
/// of every grant it stores); a blob that does not decode is left for the
/// owner's own revoke rather than guessed at.
fn end_content_write_grants_held_by(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    holder: &[u8],
    old_writer: &[u8; 32],
) -> Result<u32> {
    let old_factor = fauna_core::grant_event::writer_factor(old_writer);
    use fauna_mls::wrapped_blob::{GrantBlob, ScopeTuple};
    let held: Vec<(Vec<u8>, Vec<u8>)> = {
        let mut stmt = tx
            .prepare(
                "SELECT grant_id, blob FROM capability_grants
                  WHERE owner_actor_id = ?1 AND holder_pubkey = ?2",
            )
            .context("prepare grants held by principal")?;
        stmt.query_map(rusqlite::params![&actor_id[..], holder], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .context("query grants held by principal")?
        .collect::<std::result::Result<_, _>>()
        .context("collect grants held by principal")?
    };
    let mut ended = 0u32;
    for (grant_id, blob) in held {
        // window-ok(an ending, not an authorization: a grant whose window has
        // not opened yet names the old writer just the same)
        let writes = GrantBlob::from_canonical_bytes(&blob).is_ok_and(|b| {
            b.scope.iter().any(|t| {
                t.class == ScopeTuple::CLASS_CONTENT_WRITE
                    && t.factor.as_deref() == Some(old_factor.as_str())
            })
        });
        if writes {
            tx.execute(
                "DELETE FROM capability_grants WHERE owner_actor_id = ?1 AND grant_id = ?2",
                rusqlite::params![&actor_id[..], grant_id],
            )
            .context("end content.write grant")?;
            ended = ended.saturating_add(1);
        }
    }
    Ok(ended)
}

/// Delete every capability grant `actor_id` minted to `holder` — the same
/// DELETE the owner's own `fauna.capabilities.revoke` performs, selected by
/// holder rather than by grant id. Scoped to the OWNER: a principal is one
/// account's, so only that account's grants to its key are this act's to end.
fn end_capability_grants_held_by(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    holder: &[u8],
) -> Result<u32> {
    let n = tx
        .execute(
            "DELETE FROM capability_grants WHERE owner_actor_id = ?1 AND holder_pubkey = ?2",
            rusqlite::params![&actor_id[..], holder],
        )
        .context("end capability grants held by principal")?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

impl CacheDb {
    /// The account's principals, oldest first (`fauna.principals.list`).
    pub async fn list_third_party_principals(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<PrincipalRow>> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT p.principal_id, p.client_id, p.holder_x25519, p.execution_form,
                        p.declared_kinds, p.granted_scopes, p.created_at, p.last_used_at,
                        p.label, p.publisher_key, p.writer_ed25519,
                        p.declared_bridge, p.declared_service_auth,
                        (SELECT COUNT(*) FROM atproto_oauth_grants g
                          WHERE g.actor_id = p.actor_id
                            AND g.client_id = p.client_id
                            AND g.revoked_at IS NULL
                            AND (g.expires_at IS NULL OR g.expires_at > ?2))
                   FROM third_party_principals p
                  WHERE p.actor_id = ?1
                  ORDER BY p.created_at ASC, p.principal_id ASC",
            )
            .context("prepare list third-party principals")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..], now], |r| {
                let kinds: String = r.get(4)?;
                Ok(PrincipalRow {
                    principal_id: r.get(0)?,
                    client_id: r.get(1)?,
                    holder_x25519: r.get(2)?,
                    execution_form: r.get(3)?,
                    // A row this nest wrote itself, so an undecodable array is
                    // corruption; the honest render of a roster row is without
                    // the kinds, never hiding the connection.
                    declared_kinds: serde_json::from_str(&kinds).unwrap_or_default(),
                    granted_scopes: r.get(5)?,
                    created_at: r.get(6)?,
                    last_used_at: r.get(7)?,
                    label: r.get(8)?,
                    publisher_key: r.get(9)?,
                    writer_ed25519: r.get(10)?,
                    // As `declared_kinds`: a block this nest validated and
                    // wrote itself, so an undecodable one renders as none.
                    declared_bridge: r
                        .get::<_, Option<String>>(11)?
                        .and_then(|j| serde_json::from_str(&j).ok()),
                    // As `declared_kinds`.
                    declared_service_auth: serde_json::from_str(&r.get::<_, String>(12)?)
                        .unwrap_or_default(),
                    live_grants: u32::try_from(r.get::<_, i64>(13)?).unwrap_or(u32::MAX),
                })
            })
            .context("query third-party principals")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect third-party principals")?;
        Ok(rows)
    }

    /// The live half of one principal row a principal session dispatches
    /// under: its granted scopes (the latest ceremony's, space-separated as
    /// recorded) and its two attested keys. `None` when the account holds no
    /// such principal — revoked, or never minted. Read per RPC, so a revoke or
    /// a narrowing re-consent bites at the session's next call.
    pub async fn get_third_party_principal_reach(
        &self,
        actor_id: &[u8; 32],
        principal_id: &[u8],
    ) -> Result<Option<(String, AttestedKeys)>> {
        let conn = self.conn.lock().await;
        // A key the row holds is 32 bytes by construction (the consent wrote
        // it from a `[u8; 32]`); anything else reads as no key, which serves
        // nothing.
        let key = |k: Option<Vec<u8>>| k.and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok());
        conn.query_row(
            "SELECT granted_scopes, holder_x25519, writer_ed25519 FROM third_party_principals
              WHERE actor_id = ?1 AND principal_id = ?2",
            rusqlite::params![&actor_id[..], principal_id],
            |r| {
                Ok((
                    r.get(0)?,
                    AttestedKeys {
                        holder_x25519: key(r.get(1)?),
                        writer_ed25519: key(r.get(2)?),
                    },
                ))
            },
        )
        .optional()
        .context("read third-party principal reach")
    }

    /// Every principal row of `actor_id` whose consented document declares an
    /// `events_uri` — the webhook walk's roster (`transport.md` § Push events
    /// → *Third-party event doors*): the row, not the session registry, since
    /// a remote server with no live session is exactly who the webhook is for.
    /// Each with its granted scopes, the reach the walk filters on.
    pub async fn list_event_webhooks(&self, actor_id: &[u8; 32]) -> Result<Vec<EventWebhook>> {
        let actor = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached(
                "SELECT principal_id, client_id, granted_scopes, events_uri
                   FROM third_party_principals
                  WHERE actor_id = ?1 AND events_uri IS NOT NULL
                  ORDER BY created_at ASC, principal_id ASC",
            )
            .context("prepare list event webhooks")?;
        let rows = stmt
            .query_map(rusqlite::params![&actor[..]], |r| {
                Ok(EventWebhook {
                    principal_id: r.get(0)?,
                    client_id: r.get(1)?,
                    granted_scopes: r.get(2)?,
                    events_uri: r.get(3)?,
                })
            })
            .context("query event webhooks")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect event webhooks")?;
        Ok(rows)
    }

    /// Whether a live principal row of `actor_id` declares `kind` in its
    /// verified manifest — the first half of the rule by which the nest serves
    /// an `ext:<kind>` scope (`third-party-kinds.md` § The `ext` sub-scope).
    pub async fn principal_declares_kind(&self, actor_id: &[u8; 32], kind: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM third_party_principals p, json_each(p.declared_kinds) k
                             WHERE p.actor_id = ?1 AND k.value = ?2)",
            rusqlite::params![&actor_id[..], kind],
            |r| r.get(0),
        )
        .context("read declared kinds")
    }

    /// The id of the account's principal for `client_id` — what the principal
    /// session's upgrade binds a token to (its `fauna_actor` + `client_id`).
    /// `None` when the account holds no such principal.
    pub async fn get_third_party_principal_id(
        &self,
        actor_id: &[u8; 32],
        client_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT principal_id FROM third_party_principals
              WHERE actor_id = ?1 AND client_id = ?2",
            rusqlite::params![&actor_id[..], client_id],
            |r| r.get(0),
        )
        .optional()
        .context("read third-party principal id")
    }

    /// The publisher key the account's principal for `client_id` pinned at
    /// its last consent — what key continuity holds a later use to
    /// (`third-party-kinds.md` § The manifest). `None` for no such row, or a
    /// row consented from a document without a manifest.
    pub async fn get_third_party_principal_publisher_key(
        &self,
        actor_id: &[u8; 32],
        client_id: &str,
    ) -> Result<Option<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let key: Option<Option<Vec<u8>>> = conn
            .query_row(
                "SELECT publisher_key FROM third_party_principals
                  WHERE actor_id = ?1 AND client_id = ?2",
                rusqlite::params![&actor_id[..], client_id],
                |r| r.get(0),
            )
            .optional()
            .context("read third-party principal publisher key")?;
        // 32 bytes by construction (the consent wrote it from a `[u8; 32]`).
        Ok(key
            .flatten()
            .and_then(|k| <[u8; 32]>::try_from(k.as_slice()).ok()))
    }

    /// End a principal — the one verb (`third-party.md` § The principal model,
    /// rule 4). `None` when the account holds no such principal.
    ///
    /// **One transaction** for the whole cascade, so a revoke that fails
    /// midway ends nothing rather than leaving a roster row with half its
    /// reach gone and nothing saying which half. The grant-family writes are
    /// [`revoke_session_row`] + [`revoke_grant_row`] — the pair every
    /// revocation of a family writes — over **every** unrevoked grant for
    /// `(actor, client_id)`: each ceremony mints its own family, and a
    /// principal that consented twice holds two.
    pub async fn revoke_third_party_principal(
        &self,
        actor_id: &[u8; 32],
        principal_id: &[u8],
    ) -> Result<Option<PrincipalRevoked>> {
        let actor = *actor_id;
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin revoke principal")?;
        let revoked = revoke_principal_in_tx(&tx, &actor, principal_id, now)?;
        tx.commit().context("commit revoke principal")?;
        Ok(revoked)
    }

    /// Mint an installed plugin's INSTALL row — the install-approval leg's act
    /// (`third-party.md` § The runner contract). One transaction: the
    /// principal row under [`NEST_OWNER_ACTOR`] with `execution_form = wasm`,
    /// the manifest's `publisher_key` and `declared_kinds`, the host-minted
    /// holder key's public half; and the hosted half. Grants nothing (rule 1:
    /// install ≠ grant). Returns the new `principal_id`.
    ///
    /// # Errors
    /// [`InstallRefused::AlreadyInstalled`] when the document already has an
    /// install row — nothing is written.
    pub async fn mint_hosted_plugin(&self, mint: &HostedPluginMint) -> Result<Vec<u8>> {
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin mint hosted plugin")?;
        let installed: bool = tx
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM hosted_plugins WHERE client_id = ?1)",
                rusqlite::params![mint.client_id],
                |r| r.get(0),
            )
            .context("check plugin installed")?;
        if installed {
            return Err(anyhow!(InstallRefused::AlreadyInstalled));
        }
        refuse_foreign_holder(&tx, &NEST_OWNER_ACTOR, &mint.client_id, &mint.holder_x25519)?;
        let mut principal_id = [0u8; PRINCIPAL_ID_LEN];
        getrandom::fill(&mut principal_id).context("principal id entropy")?;
        tx.execute(
            "INSERT INTO third_party_principals
                (actor_id, principal_id, client_id, holder_x25519, execution_form,
                 declared_kinds, granted_scopes, created_at, last_used_at, label,
                 publisher_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)",
            rusqlite::params![
                &NEST_OWNER_ACTOR[..],
                &principal_id[..],
                mint.client_id,
                &mint.holder_x25519[..],
                ExecutionForm::Wasm.as_str(),
                serde_json::to_string(&mint.declared_kinds).context("encode declared kinds")?,
                mint.requested_scopes,
                now,
                mint.label,
                &mint.publisher_key[..],
            ],
        )
        .context("insert install row")?;
        tx.execute(
            "INSERT INTO hosted_plugins
                (principal_id, client_id, module_digest, hosts, ingress, settings_schema,
                 installed_by, installed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                &principal_id[..],
                mint.client_id,
                mint.module_digest,
                serde_json::to_string(&mint.hosts).context("encode hosts")?,
                serde_json::to_string(&mint.ingress).context("encode ingress")?,
                mint.settings_schema
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .context("encode settings schema")?,
                &mint.installed_by[..],
                now,
            ],
        )
        .context("insert hosted plugin")?;
        tx.commit().context("commit mint hosted plugin")?;
        Ok(principal_id.to_vec())
    }

    /// Every installed plugin, oldest first (`fauna.plugins.list`, the
    /// runner's boot walk).
    pub async fn list_hosted_plugins(&self) -> Result<Vec<HostedPluginRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT h.principal_id, h.client_id, p.label, p.holder_x25519, p.publisher_key,
                        p.declared_kinds, p.granted_scopes, h.module_digest, h.hosts, h.ingress,
                        h.settings_schema, h.installed_by, h.installed_at
                   FROM hosted_plugins h JOIN third_party_principals p
                     ON p.actor_id = ?1 AND p.principal_id = h.principal_id
                  ORDER BY h.installed_at ASC, h.principal_id ASC",
            )
            .context("prepare list hosted plugins")?;
        let mut rows = stmt
            .query_map(rusqlite::params![&NEST_OWNER_ACTOR[..]], |r| {
                let kinds: String = r.get(5)?;
                let hosts: String = r.get(8)?;
                let ingress: String = r.get(9)?;
                let schema: Option<String> = r.get(10)?;
                Ok(HostedPluginRow {
                    principal_id: r.get(0)?,
                    client_id: r.get(1)?,
                    label: r.get(2)?,
                    holder_x25519: r.get::<_, Option<Vec<u8>>>(3)?.unwrap_or_default(),
                    publisher_key: r.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                    // Rows this nest wrote itself: an undecodable member is
                    // corruption, rendered as empty rather than hiding the
                    // install.
                    declared_kinds: serde_json::from_str(&kinds).unwrap_or_default(),
                    requested_scopes: r.get(6)?,
                    module_digest: r.get(7)?,
                    hosts: serde_json::from_str(&hosts).unwrap_or_default(),
                    ingress: serde_json::from_str(&ingress).unwrap_or(serde_json::Value::Null),
                    settings_schema: schema.and_then(|s| serde_json::from_str(&s).ok()),
                    installed_by: r.get(11)?,
                    installed_at: r.get(12)?,
                    bound_accounts: Vec::new(),
                })
            })
            .context("query hosted plugins")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("collect hosted plugins")?;
        let mut bound = conn
            .prepare(
                "SELECT actor_id FROM third_party_principals
                  WHERE client_id = ?1 AND actor_id != ?2
                  ORDER BY created_at ASC",
            )
            .context("prepare bound accounts")?;
        for row in &mut rows {
            row.bound_accounts = bound
                .query_map(
                    rusqlite::params![row.client_id, &NEST_OWNER_ACTOR[..]],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .context("query bound accounts")?
                .filter_map(|r| r.ok())
                .filter_map(|a| <[u8; 32]>::try_from(a.as_slice()).ok())
                .collect();
        }
        Ok(rows)
    }

    /// The installed plugin for `client_id`, if any.
    pub async fn get_hosted_plugin(&self, client_id: &str) -> Result<Option<HostedPluginRow>> {
        Ok(self
            .list_hosted_plugins()
            .await?
            .into_iter()
            .find(|p| p.client_id == client_id))
    }

    /// Uninstall — the admin's one verb over a hosted plugin
    /// (`fauna.plugins.uninstall`): one transaction ends every account's
    /// binding row for the document through the same cascade
    /// `fauna.principals.revoke` runs (its grant families, the capability
    /// grants to the plugin's key), deletes the plugin's state, the hosted
    /// half and the install row. `None` when no such plugin is installed.
    /// Stopping the runner is the caller's act, after the commit.
    pub async fn uninstall_hosted_plugin(
        &self,
        principal_id: &[u8],
    ) -> Result<Option<PluginUninstalled>> {
        let now = now_epoch_millis();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin uninstall plugin")?;
        let client_id: Option<String> = tx
            .query_row(
                "SELECT client_id FROM hosted_plugins WHERE principal_id = ?1",
                rusqlite::params![principal_id],
                |r| r.get(0),
            )
            .optional()
            .context("read plugin to uninstall")?;
        let Some(client_id) = client_id else {
            return Ok(None);
        };
        let bindings: Vec<(Vec<u8>, Vec<u8>)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT actor_id, principal_id FROM third_party_principals
                      WHERE client_id = ?1 AND actor_id != ?2",
                )
                .context("prepare plugin bindings")?;
            stmt.query_map(rusqlite::params![client_id, &NEST_OWNER_ACTOR[..]], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .context("query plugin bindings")?
            .collect::<std::result::Result<_, _>>()
            .context("collect plugin bindings")?
        };
        let mut ended = PluginUninstalled::default();
        for (actor, binding) in &bindings {
            let Ok(actor) = <[u8; 32]>::try_from(actor.as_slice()) else {
                continue;
            };
            if let Some(r) = revoke_principal_in_tx(&tx, &actor, binding, now)? {
                ended.bindings_ended = ended.bindings_ended.saturating_add(1);
                ended.grants_ended = ended.grants_ended.saturating_add(r.grants_ended);
                ended.capability_grants_ended = ended
                    .capability_grants_ended
                    .saturating_add(r.capability_grants_ended);
            }
        }
        let n = tx
            .execute(
                "DELETE FROM plugin_state WHERE principal_id = ?1",
                rusqlite::params![principal_id],
            )
            .context("delete plugin state")?;
        ended.state_entries_deleted = u32::try_from(n).unwrap_or(u32::MAX);
        tx.execute(
            "DELETE FROM hosted_plugins WHERE principal_id = ?1",
            rusqlite::params![principal_id],
        )
        .context("delete hosted plugin")?;
        // The install row itself through the same cascade (it holds no grant
        // families; its capability-grant arm is a no-op by construction).
        revoke_principal_in_tx(&tx, &NEST_OWNER_ACTOR, principal_id, now)?;
        tx.commit().context("commit uninstall plugin")?;
        Ok(Some(ended))
    }

    /// `state.get` for a hosted plugin.
    pub async fn plugin_state_get(
        &self,
        principal_id: &[u8],
        key: &str,
    ) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT value FROM plugin_state WHERE principal_id = ?1 AND key = ?2",
            rusqlite::params![principal_id, key],
            |r| r.get(0),
        )
        .optional()
        .context("read plugin state")
    }

    /// `state.put` for a hosted plugin — upsert.
    pub async fn plugin_state_put(
        &self,
        principal_id: &[u8],
        key: &str,
        value: &[u8],
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO plugin_state (principal_id, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (principal_id, key) DO UPDATE SET value = excluded.value",
            rusqlite::params![principal_id, key, value],
        )
        .context("write plugin state")?;
        Ok(())
    }

    /// `state.delete` for a hosted plugin.
    pub async fn plugin_state_delete(&self, principal_id: &[u8], key: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM plugin_state WHERE principal_id = ?1 AND key = ?2",
            rusqlite::params![principal_id, key],
        )
        .context("delete plugin state")?;
        Ok(())
    }
}

/// The revoke cascade inside the caller's transaction — what
/// [`CacheDb::revoke_third_party_principal`] and the uninstall share. `None`
/// when `actor_id` holds no such principal.
fn revoke_principal_in_tx(
    tx: &rusqlite::Transaction<'_>,
    actor_id: &[u8; 32],
    principal_id: &[u8],
    now: i64,
) -> Result<Option<PrincipalRevoked>> {
    let found: Option<(String, Option<Vec<u8>>)> = tx
        .query_row(
            "SELECT client_id, holder_x25519 FROM third_party_principals
              WHERE actor_id = ?1 AND principal_id = ?2",
            rusqlite::params![&actor_id[..], principal_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("read principal to revoke")?;
    let Some((client_id, holder)) = found else {
        return Ok(None);
    };

    let families: Vec<Vec<u8>> = {
        let mut stmt = tx
            .prepare(
                "SELECT grant_id FROM atproto_oauth_grants
                  WHERE actor_id = ?1 AND client_id = ?2 AND revoked_at IS NULL
                  ORDER BY grant_id",
            )
            .context("prepare principal grant families")?;
        stmt.query_map(rusqlite::params![&actor_id[..], client_id], |r| r.get(0))
            .context("query principal grant families")?
            .collect::<std::result::Result<_, _>>()
            .context("collect principal grant families")?
    };
    let mut revoked = PrincipalRevoked::default();
    for family in &families {
        revoke_session_row(tx, actor_id, family, now)?;
        revoke_grant_row(tx, actor_id, family, now)?;
        revoked.grants_ended = revoked.grants_ended.saturating_add(1);
    }
    if let Some(holder) = holder {
        revoked.capability_grants_ended = end_capability_grants_held_by(tx, actor_id, &holder)?;
    }
    // The bridged rooms it served stay — they are the user's — and lose
    // nothing but the undrained outbox sealed to its key, each Sent row
    // stamped undelivered (`apps/bridges.md` § Phase G → *When the bridge
    // stops serving*). A later principal declaring the same bridge id adopts
    // them at its consent.
    revoked.outbox_rooms =
        super::bridged_conversations::end_bridged_outbox_in_tx(tx, actor_id, principal_id, now)?;
    // The oracle's bound NIP-46 client (TP11), which every request already
    // re-resolves against this row — deleted here so a revoked principal
    // leaves nothing behind. The `nostr` feature may be off, so the table
    // may not exist (the `successions.rs` guard).
    if super::table_exists(tx, "nostr_oracle_clients")? {
        let actor_hex = hex::encode(actor_id);
        tx.execute(
            "DELETE FROM nostr_oracle_ops WHERE client_id IN
               (SELECT id FROM nostr_oracle_clients
                 WHERE actor_id = ?1 AND principal_id = ?2)",
            rusqlite::params![actor_hex, principal_id],
        )
        .context("delete the principal's oracle ops")?;
        tx.execute(
            "DELETE FROM nostr_oracle_clients WHERE actor_id = ?1 AND principal_id = ?2",
            rusqlite::params![actor_hex, principal_id],
        )
        .context("delete the principal's oracle client")?;
    }
    tx.execute(
        "DELETE FROM third_party_principals WHERE actor_id = ?1 AND principal_id = ?2",
        rusqlite::params![&actor_id[..], principal_id],
    )
    .context("delete principal")?;
    Ok(Some(revoked))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: [u8; 32] = [0xA1; 32];
    const BOB: [u8; 32] = [0xB0; 32];
    const CLIENT: &str = "https://app.example/client.json";
    const NEVER: i64 = i64::MAX;

    fn attest(holder: Option<[u8; 32]>) -> PrincipalAttestation {
        PrincipalAttestation {
            keys: AttestedKeys {
                holder_x25519: holder,
                writer_ed25519: None,
            },
            execution_form: ExecutionForm::Device,
            manifest: None,
        }
    }

    async fn consent(
        db: &CacheDb,
        actor: &[u8; 32],
        grant_id: &[u8],
        client: &str,
        holder: Option<[u8; 32]>,
    ) -> Result<()> {
        db.record_atproto_oauth_grant(
            actor,
            grant_id,
            client,
            Some("Example App"),
            "atproto",
            &[],
            "jkt",
            NEVER,
            None,
            super::super::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &attest(holder),
        )
        .await
    }

    #[tokio::test]
    async fn one_document_is_one_principal_however_many_consents() {
        let db = CacheDb::open_in_memory().unwrap();
        consent(&db, &ALICE, b"family-1", CLIENT, Some([7; 32]))
            .await
            .unwrap();
        let first = db.list_third_party_principals(&ALICE).await.unwrap();
        assert_eq!(first.len(), 1);
        let row = &first[0];
        assert_eq!(row.client_id, CLIENT);
        assert_eq!(row.label.as_deref(), Some("Example App"));
        assert_eq!(row.holder_x25519.as_deref(), Some(&[7u8; 32][..]));
        assert_eq!(row.execution_form, "device");
        assert_eq!(row.granted_scopes, "atproto");
        assert!(row.declared_kinds.is_empty());
        assert_eq!(row.live_grants, 1);
        assert_eq!(row.principal_id.len(), PRINCIPAL_ID_LEN);

        consent(&db, &ALICE, b"family-2", CLIENT, Some([7; 32]))
            .await
            .unwrap();
        let again = db.list_third_party_principals(&ALICE).await.unwrap();
        assert_eq!(again.len(), 1, "a re-consent finds the row: {again:?}");
        assert_eq!(again[0].principal_id, row.principal_id);
        assert_eq!(again[0].live_grants, 2, "each ceremony is its own family");

        // Another account's consent to the same document is ITS principal.
        consent(&db, &BOB, b"family-3", CLIENT, None).await.unwrap();
        assert_eq!(
            db.list_third_party_principals(&ALICE).await.unwrap().len(),
            1
        );
        assert_eq!(db.list_third_party_principals(&BOB).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn revoke_ends_every_family_every_capability_and_the_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let holder = [9u8; 32];
        consent(&db, &ALICE, b"family-1", CLIENT, Some(holder))
            .await
            .unwrap();
        consent(&db, &ALICE, b"family-2", CLIENT, Some(holder))
            .await
            .unwrap();
        // Alice grants the principal's key; a grant she made to another holder,
        // and one Bob made to the same key, are neither this revoke's to end.
        db.put_capability_grant(&ALICE, &[1; 16], &holder, NEVER, b"g1")
            .await
            .unwrap();
        db.put_capability_grant(&ALICE, &[2; 16], &[3; 32], NEVER, b"g2")
            .await
            .unwrap();
        db.put_capability_grant(&BOB, &[4; 16], &holder, NEVER, b"g3")
            .await
            .unwrap();

        let id = db.list_third_party_principals(&ALICE).await.unwrap()[0]
            .principal_id
            .clone();
        let ended = db
            .revoke_third_party_principal(&ALICE, &id)
            .await
            .unwrap()
            .expect("the principal existed");
        assert_eq!(ended.grants_ended, 2);
        assert_eq!(ended.capability_grants_ended, 1);

        assert!(
            db.list_third_party_principals(&ALICE)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            db.list_atproto_oauth_grants(&ALICE)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(db.list_atproto_sessions(&ALICE).await.unwrap().is_empty());
        assert!(
            db.get_capability_grant(&ALICE, &[1; 16])
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_capability_grant(&ALICE, &[2; 16])
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            db.get_capability_grant(&BOB, &[4; 16])
                .await
                .unwrap()
                .is_some()
        );

        assert_eq!(
            db.revoke_third_party_principal(&ALICE, &id).await.unwrap(),
            None,
            "a second revoke finds nothing to end"
        );
    }

    #[tokio::test]
    async fn another_accounts_principal_cannot_be_revoked_by_id() {
        let db = CacheDb::open_in_memory().unwrap();
        consent(&db, &BOB, b"family-1", CLIENT, None).await.unwrap();
        let bobs = db.list_third_party_principals(&BOB).await.unwrap()[0]
            .principal_id
            .clone();
        assert_eq!(
            db.revoke_third_party_principal(&ALICE, &bobs)
                .await
                .unwrap(),
            None
        );
        assert_eq!(db.list_third_party_principals(&BOB).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_client_that_attests_no_key_keeps_the_one_it_attested_before() {
        let db = CacheDb::open_in_memory().unwrap();
        consent(&db, &ALICE, b"family-1", CLIENT, None)
            .await
            .unwrap();
        assert_eq!(
            db.list_third_party_principals(&ALICE).await.unwrap()[0].holder_x25519,
            None
        );
        consent(&db, &ALICE, b"family-2", CLIENT, Some([5; 32]))
            .await
            .unwrap();
        consent(&db, &ALICE, b"family-3", CLIENT, None)
            .await
            .unwrap();
        assert_eq!(
            db.list_third_party_principals(&ALICE).await.unwrap()[0]
                .holder_x25519
                .as_deref(),
            Some(&[5u8; 32][..])
        );
    }

    #[tokio::test]
    async fn a_new_key_ends_the_grants_held_by_the_old_one() {
        let db = CacheDb::open_in_memory().unwrap();
        consent(&db, &ALICE, b"family-1", CLIENT, Some([5; 32]))
            .await
            .unwrap();
        db.put_capability_grant(&ALICE, &[1; 16], &[5; 32], NEVER, b"g")
            .await
            .unwrap();
        consent(&db, &ALICE, b"family-2", CLIENT, Some([6; 32]))
            .await
            .unwrap();
        assert!(
            db.get_capability_grant(&ALICE, &[1; 16])
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db.list_third_party_principals(&ALICE).await.unwrap()[0]
                .holder_x25519
                .as_deref(),
            Some(&[6u8; 32][..])
        );
    }

    #[tokio::test]
    async fn a_key_another_principal_holds_is_refused_and_records_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        consent(&db, &ALICE, b"family-1", CLIENT, Some([5; 32]))
            .await
            .unwrap();
        let err = consent(
            &db,
            &ALICE,
            b"family-2",
            "https://other.example/c.json",
            Some([5; 32]),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.downcast_ref::<HolderKeyRefused>(),
            Some(&HolderKeyRefused::HeldByAnotherPrincipal)
        );
        // The grant rolled back with the principal: no connection to show.
        assert_eq!(db.list_atproto_oauth_grants(&ALICE).await.unwrap().len(), 1);
        assert_eq!(
            db.list_third_party_principals(&ALICE).await.unwrap().len(),
            1
        );
    }

    const PLUGIN: &str = "https://plugin.example/fauna-client.json";

    fn plugin_mint(key: [u8; 32]) -> HostedPluginMint {
        HostedPluginMint {
            client_id: PLUGIN.into(),
            label: Some("Matrix bridge".into()),
            holder_x25519: key,
            publisher_key: [0xEE; 32],
            declared_kinds: vec!["ext.plugin.example.room".into()],
            requested_scopes: "fauna:records:rw:ext.plugin.example.* fauna:conversations:bridge"
                .into(),
            module_digest: "sha256:00".into(),
            hosts: vec!["matrix.org".into()],
            ingress: serde_json::json!([{ "path": "/matrix/", "sni": null }]),
            settings_schema: None,
            installed_by: ALICE,
        }
    }

    /// The install row lives under the nest owner with the manifest's
    /// members and the hosted half; it is nobody's roster row; a second
    /// install of the same document refuses with nothing written.
    #[tokio::test]
    async fn an_install_mints_one_row_under_the_nest_owner() {
        let db = CacheDb::open_in_memory().unwrap();
        let id = db
            .mint_hosted_plugin(&plugin_mint([0x11; 32]))
            .await
            .unwrap();
        assert_eq!(id.len(), PRINCIPAL_ID_LEN);
        let plugins = db.list_hosted_plugins().await.unwrap();
        assert_eq!(plugins.len(), 1);
        let p = &plugins[0];
        assert_eq!(p.principal_id, id);
        assert_eq!(p.client_id, PLUGIN);
        assert_eq!(p.label.as_deref(), Some("Matrix bridge"));
        assert_eq!(p.holder_x25519, vec![0x11; 32]);
        assert_eq!(p.publisher_key, vec![0xEE; 32]);
        assert_eq!(
            p.declared_kinds,
            vec!["ext.plugin.example.room".to_string()]
        );
        assert_eq!(p.hosts, vec!["matrix.org".to_string()]);
        assert_eq!(p.ingress[0]["path"], "/matrix/");
        assert_eq!(p.installed_by, ALICE.to_vec());
        assert!(p.bound_accounts.is_empty());
        // Not the admin's connected app, nor anyone's.
        assert!(
            db.list_third_party_principals(&ALICE)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.list_third_party_principals(&NEST_OWNER_ACTOR)
                .await
                .unwrap()[0]
                .execution_form,
            "wasm"
        );

        let err = db
            .mint_hosted_plugin(&plugin_mint([0x12; 32]))
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<InstallRefused>(),
            Some(&InstallRefused::AlreadyInstalled)
        );
        assert_eq!(db.list_hosted_plugins().await.unwrap().len(), 1);
    }

    /// A user's consent to an installed plugin's document mints a BINDING
    /// row of the install's form and key — install ≠ grant, and the binding
    /// is the user's own roster row.
    #[tokio::test]
    async fn a_consent_to_an_installed_plugin_binds_the_account() {
        let db = CacheDb::open_in_memory().unwrap();
        db.mint_hosted_plugin(&plugin_mint([0x11; 32]))
            .await
            .unwrap();
        consent(&db, &ALICE, b"family-1", PLUGIN, None)
            .await
            .unwrap();
        let alice = db.list_third_party_principals(&ALICE).await.unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].execution_form, "wasm");
        assert_eq!(alice[0].holder_x25519.as_deref(), Some(&[0x11u8; 32][..]));
        assert_eq!(alice[0].live_grants, 1);
        assert_eq!(
            db.list_hosted_plugins().await.unwrap()[0].bound_accounts,
            vec![ALICE]
        );
        // Bob binding too: the same key on another account is fine (the
        // uniqueness is per account).
        consent(&db, &BOB, b"family-2", PLUGIN, None).await.unwrap();
        assert_eq!(
            db.list_hosted_plugins().await.unwrap()[0].bound_accounts,
            vec![ALICE, BOB]
        );
        // A user's own revoke ends only their binding; the plugin stays.
        let id = alice[0].principal_id.clone();
        db.revoke_third_party_principal(&ALICE, &id).await.unwrap();
        assert_eq!(
            db.list_hosted_plugins().await.unwrap()[0].bound_accounts,
            vec![BOB]
        );
    }

    /// Uninstall ends every binding through the revoke cascade, the state,
    /// the hosted half and the install row — one transaction, one verb.
    #[tokio::test]
    async fn uninstall_ends_every_binding_the_state_and_the_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let key = [0x11u8; 32];
        let id = db.mint_hosted_plugin(&plugin_mint(key)).await.unwrap();
        consent(&db, &ALICE, b"family-1", PLUGIN, None)
            .await
            .unwrap();
        consent(&db, &BOB, b"family-2", PLUGIN, None).await.unwrap();
        db.put_capability_grant(&ALICE, &[1; 16], &key, NEVER, b"g1")
            .await
            .unwrap();
        db.plugin_state_put(&id, "cursor", b"42").await.unwrap();
        db.plugin_state_put(&id, "cursor", b"43").await.unwrap();
        assert_eq!(
            db.plugin_state_get(&id, "cursor").await.unwrap(),
            Some(b"43".to_vec())
        );
        db.plugin_state_put(&id, "other", b"x").await.unwrap();
        db.plugin_state_delete(&id, "other").await.unwrap();
        assert_eq!(db.plugin_state_get(&id, "other").await.unwrap(), None);

        let ended = db
            .uninstall_hosted_plugin(&id)
            .await
            .unwrap()
            .expect("installed");
        assert_eq!(ended.bindings_ended, 2);
        assert_eq!(ended.grants_ended, 2);
        assert_eq!(ended.capability_grants_ended, 1);
        assert_eq!(ended.state_entries_deleted, 1);
        assert!(db.list_hosted_plugins().await.unwrap().is_empty());
        assert!(
            db.list_third_party_principals(&ALICE)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            db.list_third_party_principals(&BOB)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            db.list_third_party_principals(&NEST_OWNER_ACTOR)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            db.list_atproto_oauth_grants(&ALICE)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            db.get_capability_grant(&ALICE, &[1; 16])
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(db.plugin_state_get(&id, "cursor").await.unwrap(), None);
        assert_eq!(db.uninstall_hosted_plugin(&id).await.unwrap(), None);
    }

    #[test]
    fn execution_form_round_trips() {
        for form in [
            ExecutionForm::Remote,
            ExecutionForm::Device,
            ExecutionForm::Wasm,
        ] {
            assert_eq!(ExecutionForm::parse(form.as_str()), Some(form));
        }
        assert_eq!(ExecutionForm::parse("container"), None);
        assert!(ExecutionForm::Wasm.is_hosted());
        assert!(!ExecutionForm::Device.is_hosted());
    }

    #[tokio::test]
    async fn a_bridges_key_is_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO bridge_service_users
                    (ed25519_pubkey, x25519_pubkey, role, bridge_id, status, created_at)
                 VALUES (?1, ?2, 'content-processor', 'web-serve', 'approved', 0)",
                rusqlite::params![&[0xEFu8; 32][..], &[5u8; 32][..]],
            )
            .unwrap();
        }
        let err = consent(&db, &ALICE, b"family-1", CLIENT, Some([5; 32]))
            .await
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<HolderKeyRefused>(),
            Some(&HolderKeyRefused::HeldByABridge)
        );
        assert!(
            db.list_third_party_principals(&ALICE)
                .await
                .unwrap()
                .is_empty()
        );
    }

    // ── The writer key and the manifest (`third-party-kinds.md` § Principal
    // write authority, § The manifest) ──────────────────────────────────────

    const HOLDER: [u8; 32] = [5; 32];
    const WRITER: [u8; 32] = [0x57; 32];
    const KIND: &str = "ext.app.example.notes";

    async fn consent_as(
        db: &CacheDb,
        grant_id: &[u8],
        client: &str,
        keys: AttestedKeys,
        manifest: Option<PrincipalManifest>,
    ) -> Result<()> {
        db.record_atproto_oauth_grant(
            &ALICE,
            grant_id,
            client,
            Some("Example App"),
            "atproto",
            &[],
            "jkt",
            NEVER,
            None,
            super::super::atproto_pds::OAUTH_GRANT_ISSUER_NEST,
            &PrincipalAttestation {
                keys,
                execution_form: ExecutionForm::Device,
                manifest,
            },
        )
        .await
    }

    fn both(writer: [u8; 32]) -> AttestedKeys {
        AttestedKeys {
            holder_x25519: Some(HOLDER),
            writer_ed25519: Some(writer),
        }
    }

    fn grant_blob(scope: Vec<fauna_mls::wrapped_blob::ScopeTuple>) -> Vec<u8> {
        use fauna_mls::wrapped_blob::{GrantBlob, GrantIndex, GrantWindow};
        GrantBlob {
            version: 1,
            kind: GrantBlob::KIND.to_string(),
            index: GrantIndex(ALICE.to_vec(), vec![1; 16]),
            holder: serde_bytes::ByteBuf::from(HOLDER.to_vec()),
            window: GrantWindow(0, u64::MAX),
            scope,
            wrapped_keys: Vec::new(),
        }
        .to_canonical_bytes()
        .expect("encode grant blob")
    }

    #[tokio::test]
    async fn the_writer_key_and_the_manifest_ride_to_the_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let feed = ServiceAuthEntry {
            aud: "did:web:api.bsky.app#bsky_appview".into(),
            lxm: vec!["app.bsky.feed.getFeedSkeleton".into()],
            extra: Default::default(),
        };
        let manifest = PrincipalManifest {
            publisher_key: [0x9B; 32],
            declared_kinds: vec![KIND.to_string()],
            declared_service_auth: vec![feed.clone()],
            ..Default::default()
        };
        consent_as(&db, b"family-1", CLIENT, both(WRITER), Some(manifest))
            .await
            .unwrap();
        let row = &db.list_third_party_principals(&ALICE).await.unwrap()[0];
        assert_eq!(row.writer_ed25519.as_deref(), Some(&WRITER[..]));
        assert_eq!(row.publisher_key.as_deref(), Some(&[0x9B; 32][..]));
        assert_eq!(row.declared_kinds, vec![KIND.to_string()]);
        assert_eq!(row.declared_service_auth, vec![feed.clone()]);
        // The key the refresh grant holds the row to (key continuity).
        assert_eq!(
            db.get_third_party_principal_publisher_key(&ALICE, CLIENT)
                .await
                .unwrap(),
            Some([0x9B; 32])
        );
        assert_eq!(
            db.get_third_party_principal_publisher_key(&BOB, CLIENT)
                .await
                .unwrap(),
            None
        );
        assert!(db.principal_declares_kind(&ALICE, KIND).await.unwrap());
        assert!(
            !db.principal_declares_kind(&ALICE, "ext.app.example.other")
                .await
                .unwrap()
        );
        assert!(!db.principal_declares_kind(&BOB, KIND).await.unwrap());
        let (_, keys) = db
            .get_third_party_principal_reach(&ALICE, &row.principal_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(keys, both(WRITER));

        // A later ceremony's document replaces the declared set.
        let blog = ServiceAuthEntry {
            aud: "did:plc:abcdefghijklmnopqrstuvwx".into(),
            lxm: vec!["com.example.blog.getPost".into()],
            extra: Default::default(),
        };
        let later = PrincipalManifest {
            publisher_key: [0x9B; 32],
            declared_kinds: vec![KIND.to_string()],
            declared_service_auth: vec![blog.clone()],
            ..Default::default()
        };
        consent_as(&db, b"family-1b", CLIENT, both(WRITER), Some(later))
            .await
            .unwrap();
        let row = &db.list_third_party_principals(&ALICE).await.unwrap()[0];
        assert_eq!(row.declared_service_auth, vec![blog]);

        // The latest document is what the row indexes: one without a manifest
        // clears it, and silence about the writer keeps the writer.
        consent_as(&db, b"family-2", CLIENT, AttestedKeys::default(), None)
            .await
            .unwrap();
        let row = &db.list_third_party_principals(&ALICE).await.unwrap()[0];
        assert_eq!(row.writer_ed25519.as_deref(), Some(&WRITER[..]));
        assert_eq!(row.publisher_key, None);
        assert!(row.declared_kinds.is_empty());
        assert!(row.declared_service_auth.is_empty());
        assert_eq!(
            db.get_third_party_principal_publisher_key(&ALICE, CLIENT)
                .await
                .unwrap(),
            None,
            "a row consented without a manifest pins nothing"
        );
        assert!(!db.principal_declares_kind(&ALICE, KIND).await.unwrap());
    }

    fn bridge_manifest(id: &str) -> PrincipalManifest {
        use fauna_protocol::kind_manifest::{BridgeBlock, BridgeCapabilityValue};
        PrincipalManifest {
            publisher_key: [0x9B; 32],
            declared_kinds: vec![],
            declared_service_auth: vec![],
            events_uri: None,
            bridge: Some(BridgeBlock {
                id: id.into(),
                glyph: "bridge".into(),
                address_grammar: "^@[^:]+:.+$".into(),
                capabilities: [(
                    "delivery_mode".to_string(),
                    BridgeCapabilityValue::Mode("Async".into()),
                )]
                .into_iter()
                .collect(),
                extra: Default::default(),
            }),
        }
    }

    /// `fauna-conversations` never depends on `fauna-protocol`, so the
    /// manifest's capability members are listed there by name; this is the pin
    /// that they are exactly the `ThreadCapabilities` record's fields minus
    /// `encryption`, required where the record has no serde default.
    #[test]
    fn bridge_capability_members_are_thread_capabilities_minus_encryption() {
        use fauna_protocol::kind_manifest::{
            BRIDGE_CAPABILITY_OPTIONAL, BRIDGE_CAPABILITY_REQUIRED,
        };
        let mut declared: serde_json::Map<String, serde_json::Value> = BRIDGE_CAPABILITY_REQUIRED
            .iter()
            .map(|m| {
                let v = if *m == "delivery_mode" {
                    serde_json::json!("Async")
                } else {
                    serde_json::json!(true)
                };
                ((*m).to_string(), v)
            })
            .collect();
        declared.insert("encryption".into(), serde_json::json!("TransportOnly"));
        // The required set alone deserializes: nothing the record requires is
        // missing from it.
        let caps: fauna_conversations::ThreadCapabilities =
            serde_json::from_value(serde_json::Value::Object(declared)).unwrap();
        // And the record's full field set is required ∪ optional ∪ encryption.
        let serde_json::Value::Object(fields) = serde_json::to_value(caps).unwrap() else {
            panic!("a record serializes as an object");
        };
        let mut got: Vec<&str> = fields.keys().map(String::as_str).collect();
        got.sort_unstable();
        let mut want: Vec<&str> = BRIDGE_CAPABILITY_REQUIRED
            .iter()
            .chain(BRIDGE_CAPABILITY_OPTIONAL)
            .copied()
            .chain(["encryption"])
            .collect();
        want.sort_unstable();
        assert_eq!(got, want);
    }

    /// `transport.md` § Push events → *Third-party event doors*, the
    /// webhook: the consented document's `events_uri` rides to the row, the
    /// walk lists only rows that declare one, and the latest consent wins —
    /// a document that stops declaring it has no webhook.
    #[tokio::test]
    async fn the_events_uri_rides_to_the_row_and_the_latest_consent_wins() {
        let db = CacheDb::open_in_memory().unwrap();
        let with_hook = PrincipalManifest {
            publisher_key: [0x9B; 32],
            events_uri: Some("https://app.example/fauna/events".into()),
            ..Default::default()
        };
        consent_as(&db, b"family-1", CLIENT, both(WRITER), Some(with_hook))
            .await
            .unwrap();
        consent_as(
            &db,
            b"family-2",
            "https://other.example/client.json",
            AttestedKeys::default(),
            Some(PrincipalManifest::default()),
        )
        .await
        .unwrap();
        let hooks = db.list_event_webhooks(&ALICE).await.unwrap();
        assert_eq!(hooks.len(), 1, "only the row declaring one: {hooks:?}");
        assert_eq!(hooks[0].client_id, CLIENT);
        assert_eq!(hooks[0].events_uri, "https://app.example/fauna/events");
        assert_eq!(hooks[0].granted_scopes, "atproto");

        let without = PrincipalManifest {
            publisher_key: [0x9B; 32],
            ..Default::default()
        };
        consent_as(&db, b"family-1b", CLIENT, both(WRITER), Some(without))
            .await
            .unwrap();
        assert!(db.list_event_webhooks(&ALICE).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_bridge_block_rides_to_the_row_and_its_id_is_unique_per_account() {
        let db = CacheDb::open_in_memory().unwrap();
        consent_as(
            &db,
            b"family-1",
            CLIENT,
            AttestedKeys::default(),
            Some(bridge_manifest("matrix")),
        )
        .await
        .unwrap();
        let row = &db.list_third_party_principals(&ALICE).await.unwrap()[0];
        assert_eq!(row.declared_bridge, bridge_manifest("matrix").bridge);

        // Re-consenting the same document keeps its own id.
        consent_as(
            &db,
            b"family-2",
            CLIENT,
            AttestedKeys::default(),
            Some(bridge_manifest("matrix")),
        )
        .await
        .unwrap();
        // Another document claiming the same id on this account refuses, and
        // nothing is recorded for it.
        let other = "https://other.example/client.json";
        let err = consent_as(
            &db,
            b"family-3",
            other,
            AttestedKeys::default(),
            Some(bridge_manifest("matrix")),
        )
        .await
        .expect_err("a second principal may not serve as the same bridge");
        assert_eq!(
            err.downcast_ref::<HolderKeyRefused>(),
            Some(&HolderKeyRefused::BridgeIdHeldByAnotherPrincipal)
        );
        assert_eq!(
            db.list_third_party_principals(&ALICE).await.unwrap().len(),
            1
        );
        // A different id is a different bridge.
        consent_as(
            &db,
            b"family-4",
            other,
            AttestedKeys::default(),
            Some(bridge_manifest("signal")),
        )
        .await
        .unwrap();
        // A later document without the block clears it, freeing the id.
        consent_as(&db, b"family-5", CLIENT, AttestedKeys::default(), None)
            .await
            .unwrap();
        let rows = db.list_third_party_principals(&ALICE).await.unwrap();
        assert_eq!(
            rows.iter().filter(|r| r.declared_bridge.is_some()).count(),
            1
        );
    }

    #[tokio::test]
    async fn a_new_writer_key_ends_only_the_grants_that_carry_content_write() {
        use fauna_mls::wrapped_blob::ScopeTuple;
        let db = CacheDb::open_in_memory().unwrap();
        consent_as(&db, b"family-1", CLIENT, both(WRITER), None)
            .await
            .unwrap();
        let writes = grant_blob(vec![
            ScopeTuple::ext_kind_read(KIND),
            ScopeTuple::content_write(KIND, &WRITER),
        ]);
        let reads = grant_blob(vec![ScopeTuple::ext_kind_read(KIND)]);
        db.put_capability_grant(&ALICE, &[1; 16], &HOLDER, NEVER, &writes)
            .await
            .unwrap();
        db.put_capability_grant(&ALICE, &[2; 16], &HOLDER, NEVER, &reads)
            .await
            .unwrap();
        // What the replacing consent's approve deposits before the redemption:
        // a grant licensing the NEW writer.
        let new_writes = grant_blob(vec![
            ScopeTuple::ext_kind_read(KIND),
            ScopeTuple::content_write(KIND, &[0x58; 32]),
        ]);
        db.put_capability_grant(&ALICE, &[3; 16], &HOLDER, NEVER, &new_writes)
            .await
            .unwrap();
        // The same writer again ends nothing.
        consent_as(&db, b"family-2", CLIENT, both(WRITER), None)
            .await
            .unwrap();
        assert!(
            db.get_capability_grant(&ALICE, &[1; 16])
                .await
                .unwrap()
                .is_some()
        );

        consent_as(&db, b"family-3", CLIENT, both([0x58; 32]), None)
            .await
            .unwrap();
        assert!(
            db.get_capability_grant(&ALICE, &[1; 16])
                .await
                .unwrap()
                .is_none(),
            "the grant naming the old writer ends with it"
        );
        assert!(
            db.get_capability_grant(&ALICE, &[2; 16])
                .await
                .unwrap()
                .is_some(),
            "a read-only grant names no writer and stays"
        );
        assert!(
            db.get_capability_grant(&ALICE, &[3; 16])
                .await
                .unwrap()
                .is_some(),
            "the approve's own grant to the new writer survives the redemption"
        );
        assert_eq!(
            db.list_third_party_principals(&ALICE).await.unwrap()[0]
                .writer_ed25519
                .as_deref(),
            Some(&[0x58u8; 32][..])
        );
    }

    #[tokio::test]
    async fn a_writer_key_another_principal_holds_is_refused_and_records_nothing() {
        let db = CacheDb::open_in_memory().unwrap();
        consent_as(&db, b"family-1", CLIENT, both(WRITER), None)
            .await
            .unwrap();
        let other = AttestedKeys {
            holder_x25519: Some([6; 32]),
            writer_ed25519: Some(WRITER),
        };
        let err = consent_as(
            &db,
            b"family-2",
            "https://other.example/c.json",
            other,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.downcast_ref::<HolderKeyRefused>(),
            Some(&HolderKeyRefused::WriterHeldByAnotherPrincipal)
        );
        assert_eq!(
            db.list_third_party_principals(&ALICE).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn a_writer_key_the_nest_knows_is_refused() {
        let writer_only = |w: [u8; 32]| AttestedKeys {
            holder_x25519: None,
            writer_ed25519: Some(w),
        };
        let refused = |err: anyhow::Error| {
            assert_eq!(
                err.downcast_ref::<HolderKeyRefused>(),
                Some(&HolderKeyRefused::WriterKnown)
            );
        };
        let db = CacheDb::open_in_memory().unwrap();
        // The account's own identity key.
        refused(
            consent_as(&db, b"family-1", CLIENT, writer_only(ALICE), None)
                .await
                .unwrap_err(),
        );

        // A device that already wrote a row on the account's state feed.
        let device = [0xDE; 32];
        let fs = db
            .get_or_create_state_scope(&ALICE, fauna_protocol::account_state::ACCOUNT_STATE_SCOPE)
            .await
            .unwrap();
        db.record_account_state_entry(
            &ALICE,
            fs,
            &[1; 32],
            &device,
            1,
            "state-put",
            b"e",
            None,
            &[],
        )
        .await
        .unwrap()
        .unwrap();
        refused(
            consent_as(&db, b"family-1", CLIENT, writer_only(device), None)
                .await
                .unwrap_err(),
        );

        // An enrolled bridge's signing key.
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO bridge_service_users
                    (ed25519_pubkey, x25519_pubkey, role, bridge_id, status, created_at)
                 VALUES (?1, NULL, 'content-processor', 'web-serve', 'approved', 0)",
                rusqlite::params![&[0xEFu8; 32][..]],
            )
            .unwrap();
        }
        refused(
            consent_as(&db, b"family-1", CLIENT, writer_only([0xEF; 32]), None)
                .await
                .unwrap_err(),
        );
        assert!(
            db.list_third_party_principals(&ALICE)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// `apps/bridges.md` § Phase G → *When the bridge stops serving*, the
    /// consent side: the rooms follow the bridge id through a revoke, a
    /// re-consent of the same document (a new principal id), a different
    /// document claiming the freed id, a manifest that drops the block, and
    /// a holder-key replacement — each ending the undrained outbox and
    /// nothing else.
    #[tokio::test]
    async fn a_bridges_rooms_follow_the_bridge_id_through_revoke_and_re_consent() {
        use super::super::bridged_conversations::{BridgeSeat, RoomShape};
        const KEY_2: [u8; 32] = [6; 32];
        const KEY_3: [u8; 32] = [7; 32];
        const KEY_4: [u8; 32] = [8; 32];
        let other = "https://other.example/client.json";
        let db = CacheDb::open_in_memory().unwrap();
        let holder = |k: [u8; 32]| AttestedKeys {
            holder_x25519: Some(k),
            writer_ed25519: None,
        };
        let principal = |rows: Vec<PrincipalRow>, client: &str| {
            rows.into_iter()
                .find(|r| r.client_id == client)
                .map(|r| r.principal_id)
        };
        async fn room_of(db: &CacheDb) -> super::super::bridged_conversations::BridgedRoomRow {
            db.list_bridged_rooms(&ALICE).await.unwrap().remove(0)
        }
        async fn seat_of(db: &CacheDb, room_id: Vec<u8>) -> Vec<u8> {
            let conn = db.conn.lock().await;
            conn.query_row(
                "SELECT principal_id FROM room_members
                  WHERE room_id = ?1 AND principal_kind = 'bridge'",
                [&room_id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .unwrap()
        }

        // One principal serves `matrix`; a room is born on it with one
        // undrained item.
        consent_as(
            &db,
            b"f-1",
            CLIENT,
            holder(HOLDER),
            Some(bridge_manifest("matrix")),
        )
        .await
        .unwrap();
        let p1 = principal(
            db.list_third_party_principals(&ALICE).await.unwrap(),
            CLIENT,
        )
        .unwrap();
        let manifest = bridge_manifest("matrix");
        let block = manifest.bridge.as_ref().unwrap();
        let (room, _) = db
            .upsert_bridged_room(
                &ALICE,
                &BridgeSeat {
                    principal_id: &p1,
                    bridge: block,
                    bridge_x25519: &HOLDER,
                },
                "!r:example.org",
                &RoomShape::default(),
            )
            .await
            .unwrap();
        let stranded = db
            .send_bridged_message(&ALICE, &room, b"to-bridge", b"to-self")
            .await
            .unwrap();

        // Revoke: the room stays on the dead principal, the outbox is gone,
        // the Sent row says so.
        let ended = db
            .revoke_third_party_principal(&ALICE, &p1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ended.outbox_rooms, vec![room.room_id.clone()]);
        assert!(
            db.fetch_bridged_outbox(&ALICE, &p1, 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
        let r = room_of(&db).await;
        assert_eq!(r.bridge_principal_id, p1);
        assert_eq!(seat_of(&db, r.room_id.clone()).await, p1);
        let sent = &db
            .fetch_bridged_inbox(&ALICE, Some(&r.room_id), 0, 10)
            .await
            .unwrap()[0];
        assert_eq!((sent.id, sent.undelivered), (stranded, true));

        // The same document again is a NEW principal id (the row was
        // deleted) — and it adopts the room, key refreshed.
        consent_as(
            &db,
            b"f-2",
            CLIENT,
            holder(KEY_2),
            Some(bridge_manifest("matrix")),
        )
        .await
        .unwrap();
        let p2 = principal(
            db.list_third_party_principals(&ALICE).await.unwrap(),
            CLIENT,
        )
        .unwrap();
        assert_ne!(p2, p1);
        let r = room_of(&db).await;
        assert_eq!(r.bridge_principal_id, p2);
        assert_eq!(r.bridge_x25519, KEY_2);
        assert_eq!(seat_of(&db, r.room_id.clone()).await, p2);

        // A different document cannot take the id while p2 serves it …
        assert!(
            consent_as(
                &db,
                b"f-3",
                other,
                holder(KEY_3),
                Some(bridge_manifest("matrix"))
            )
            .await
            .is_err()
        );
        // … but adopts the room once the id is freed.
        db.revoke_third_party_principal(&ALICE, &p2)
            .await
            .unwrap()
            .unwrap();
        consent_as(
            &db,
            b"f-4",
            other,
            holder(KEY_3),
            Some(bridge_manifest("matrix")),
        )
        .await
        .unwrap();
        let p3 = principal(db.list_third_party_principals(&ALICE).await.unwrap(), other).unwrap();
        let r = room_of(&db).await;
        assert_eq!(r.bridge_principal_id, p3);
        assert_eq!(r.bridge_x25519, KEY_3);
        assert_eq!(seat_of(&db, r.room_id.clone()).await, p3);

        // A re-consent whose manifest drops the block ends the outbox too
        // (the principal lives on, serving nothing).
        let queued = db
            .send_bridged_message(&ALICE, &r, b"to-bridge", b"to-self")
            .await
            .unwrap();
        consent_as(&db, b"f-5", other, holder(KEY_3), None)
            .await
            .unwrap();
        assert!(
            db.fetch_bridged_outbox(&ALICE, &p3, 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
        let rows = db
            .fetch_bridged_inbox(&ALICE, Some(&r.room_id), 0, 10)
            .await
            .unwrap();
        assert!(rows.iter().any(|m| m.id == queued && m.undelivered));
        assert_eq!(
            principal(db.list_third_party_principals(&ALICE).await.unwrap(), other).unwrap(),
            p3
        );

        // Declaring it again re-adopts; a holder-key replacement under the
        // same declaration ends what was sealed to the old key.
        consent_as(
            &db,
            b"f-6",
            other,
            holder(KEY_3),
            Some(bridge_manifest("matrix")),
        )
        .await
        .unwrap();
        let r = room_of(&db).await;
        let queued = db
            .send_bridged_message(&ALICE, &r, b"to-bridge", b"to-self")
            .await
            .unwrap();
        consent_as(
            &db,
            b"f-7",
            other,
            holder(KEY_4),
            Some(bridge_manifest("matrix")),
        )
        .await
        .unwrap();
        assert!(
            db.fetch_bridged_outbox(&ALICE, &p3, 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
        let r = room_of(&db).await;
        assert_eq!(r.bridge_x25519, KEY_4);
        let rows = db
            .fetch_bridged_inbox(&ALICE, Some(&r.room_id), 0, 10)
            .await
            .unwrap();
        assert!(rows.iter().any(|m| m.id == queued && m.undelivered));
        // Rooms and every message survived all of it.
        assert_eq!(db.list_bridged_rooms(&ALICE).await.unwrap().len(), 1);
        assert_eq!(rows.len(), 3);
    }
}
