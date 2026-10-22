//! The handlers: each tool is a verb of its page's shared manager, called over
//! the shell's principal session (`apps/cli.md` § One catalog, many
//! front-ends).
//!
//! **A tool's error is the manager's error, verbatim** — a refused kind comes
//! back as an MCP tool error carrying the nest's code, never swallowed into an
//! empty success. A tool result carries the reply as **data**; the catalog adds
//! no instruction of its own to any result (§ Prompt-injection posture). The one
//! text addressed to anyone is the connect answer and the not-connected
//! refusal, which address the HUMAN with the step only they can take
//! (§ The device shell → *The connect tool*).

use fauna_client_admin::admin::{
    AdminMembershipTierSetRequest, AdminTierCreateRequest, AdminTierUpdateRequest,
};
use fauna_client_admin::{AdminClient, RegionCode, UpdateChannel, WebAppOrigin};
use fauna_client_bridges::MailAdminClient;
use fauna_client_features::{AuthoringTier, FeaturesClient, GatedFeature, SaveError};
use fauna_client_feed::FeedClient;
use fauna_client_moderation::ModerationClient;
use fauna_client_notifications::NotificationsClient;
use fauna_client_profile::{ProfileClient, ProfileDisplay, decode_profile_display};
use fauna_client_web::WebClient;
use fauna_core::content_policy::{ContentPolicyDocument, decode_document};
use fauna_core::data::PostBody;
use fauna_core::feature_gate::FeaturePolicy;
use fauna_protocol::ByteBuf;
use fauna_protocol::age::AgeBand;
use fauna_protocol::bridge_routing::ListForwardersReply;
use fauna_protocol::bridge_routing::{
    AddLocalDomainReply, AddLocalDomainRequest, DEFAULT_CERT_MODE, DmarcMode,
    GetPrimaryDomainRenameStatusReply, GetPrimaryDomainRenameStatusRequest, ListLocalDomainsReply,
    ListLocalDomainsRequest, ListPrimaryDomainRenamesReply, ListPrimaryDomainRenamesRequest,
    RemoveLocalDomainReply, RemoveLocalDomainRequest, RestoreLocalDomainReply,
    RestoreLocalDomainRequest, RoleAddressKind, SetCatchAllActorReply, SetCatchAllActorRequest,
    SetDkimRotationDaysReply, SetDkimRotationDaysRequest, SetRoleAddressReply,
    SetRoleAddressRequest, UpdateLocalDomainConfigReply, UpdateLocalDomainConfigRequest,
};
use fauna_protocol::discovery::SetupStatusReply;
use fauna_protocol::dns::{
    ListRecordsReply, ListRecordsRequest, VerifyRecordsReply, VerifyRecordsRequest,
};
use fauna_protocol::moderation::AbuseReportOutcome;
use fauna_protocol::node_policy::RegistrationMode;
use fauna_protocol::post_author::{
    KIND_CREATE, KIND_DELETE, PostAuthorCreateReply, PostAuthorCreateRequest,
    PostAuthorDeleteReply, PostAuthorDeleteRequest,
};
use fauna_protocol::profile_author::{KIND_SET, ProfileAuthorSetReply, ProfileAuthorSetRequest};
use fauna_protocol::tls::{CertStatusReply, CertStatusRequest};
use fauna_protocol::wrapped_blob::{ListDkimSelectorsReply, ListDkimSelectorsRequest};
use fauna_protocol::{RpcErrorClass, RpcRequester};
use serde::Serialize;
use serde_json::{Value, json};

use crate::catalog::{self, Resource, Tool, ToolId};

/// Why a shell has no principal session to call through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionUnavailable {
    /// No consent has started: the human has to be asked for their handle.
    NotConnected,
    /// A consent is waiting on the human to type `user_code`.
    Pending { user_code: String },
    /// The connection failed or was revoked; the text says what happened.
    Failed(String),
    /// The shell holds several accounts and the call named none of them; the
    /// handles it holds, for the host to ask the user which.
    AccountNeeded { held: Vec<String> },
    /// The call named an account the shell does not hold.
    UnknownAccount { account: String, held: Vec<String> },
}

/// What a started consent hands back for the human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectStarted {
    /// The account this consent is for, as the user named it — what a tool's
    /// `account` argument takes once the shell holds more than one.
    pub account: String,
    /// The code the human types into *Connect an app* on one of their apps.
    pub user_code: String,
    /// The nest the handle resolved to.
    pub nest: String,
    /// Seconds until the code expires.
    pub expires_in: u64,
    /// [`UNATTENDED_CUSTODY`] when the shell's credentials rest in the
    /// unattended file arm — the declaration the user is owed (`apps/cli.md`
    /// § The device shell → *Credentials*); absent under an OS store.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custody: Option<&'static str>,
}

/// The one sentence that declares the unattended credential arm, reused by
/// every answer and log line that mentions custody (`apps/cli.md` § The
/// device shell → *Credentials*).
// public-text: cli-answer-strings-public
pub const UNATTENDED_CUSTODY: &str = "This connection is stored on this computer's disk, protected by file permissions alone rather than by a keychain or secret store; Disconnect on its row in Settings → Connected apps, in any of your Fauna apps, ends it.";
// public-text-end: cli-answer-strings-public

/// One operation of the conversations oracle's two classes, as the catalog
/// asks a shell to perform it at the custodian's door
/// (`conversations-oracle.md` § The two classes). No membership operation
/// exists, and none may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationOp {
    /// `conversation.read`: the account's rooms.
    ListRooms,
    /// `conversation.read`: one page of a room, before `before` or the newest.
    ReadRoom {
        room_id: String,
        before: Option<u64>,
    },
    /// `conversation.read`: the device's index, or one community room.
    Search {
        query: String,
        room_id: Option<String>,
    },
    /// `conversation.send`: a text message as the account.
    Send { room_id: String, body: String },
}

/// A tool's reply: JSON data, as the MCP result carries it.
pub type ToolData = Value;

/// What a shell with no door answers a conversations tool: the nest-hosted
/// shell has no device and no custodian (`conversations-oracle.md` § What
/// each shell serves).
// public-text: cli-answer-strings-public
pub const NO_CONVERSATIONS_DOOR: &str = "Conversations are not reachable from here: they are read and sent only through a Fauna app signed in on the user's own device.";
// public-text-end: cli-answer-strings-public

/// What a shell supplies (architectural rule 3: a shell is a transport and a
/// host, nothing more).
///
/// **Accounts.** A shell may hold several principals, one per account
/// (`apps/cli.md` § The device shell). Every call names the one it acts for as
/// `account` — the handle the user connected, or `None` when the shell holds
/// exactly one; with several held, `None` is [`SessionUnavailable::AccountNeeded`].
#[allow(async_fn_in_trait)] // static dispatch only, per-impl `Send` inference — the `RpcRequester` reasoning
pub trait Host {
    /// The principal session's transport — a cheap handle (an `Arc`), so a
    /// lost one can be named back to [`Host::redial`].
    type Rpc: RpcRequester<Error: RpcErrorClass> + Clone;

    /// The live principal session for `account`.
    async fn session(&self, account: Option<&str>) -> Result<Self::Rpc, SessionUnavailable>;

    /// The account the live session acts for, as its hex actor id — what
    /// `nest://profile` reads. The device shell has it from the token answer's
    /// `sub`, which for a Fauna-family grant is the hex actor id.
    async fn account(&self, account: Option<&str>) -> Result<String, SessionUnavailable>;

    /// `lost` — the session [`Host::session`] handed out — dropped under a
    /// call: dial `account`'s session again, once, and answer the new one. A
    /// session some other call already replaced is answered as it stands, so
    /// concurrent calls that saw one drop dial once between them.
    async fn redial(
        &self,
        account: Option<&str>,
        lost: &Self::Rpc,
    ) -> Result<Self::Rpc, SessionUnavailable>;

    /// Start the consent ceremony for `handle` (the typed-code start on the
    /// device shell) and answer the code the human types; `admin` asks for
    /// the admin arms too ([`crate::catalog::consent_scope`]).
    async fn connect(&self, handle: &str, admin: bool) -> Result<ConnectStarted, ToolError>;

    /// The nest's public setup status for `account`, read on an anonymous
    /// pre-identity connection: `fauna.setup.status` is no principal arm's
    /// kind (the `ThirdParty` ceiling holds none of the pre-identity kinds),
    /// and it needs none — the answer is public.
    async fn setup_status(&self, account: Option<&str>) -> Result<SetupStatusReply, ToolError>;

    /// Perform `op` for `account` at the conversations oracle's door — the
    /// signed-in first-party app on this device, which holds the keys this
    /// shell never does — and answer its reply as data, or its refusal's
    /// sentence verbatim. A shell with no door answers
    /// [`NO_CONVERSATIONS_DOOR`].
    async fn conversation(
        &self,
        account: Option<&str>,
        op: ConversationOp,
    ) -> Result<ToolData, ToolError> {
        let _ = (account, op);
        Err(ToolError::new(NO_CONVERSATIONS_DOOR))
    }
}

/// One tool's failure, as the MCP tool error carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    /// The nest's error code when the nest refused, verbatim.
    pub code: Option<String>,
    pub message: String,
    /// The connection went away under the call
    /// ([`RpcErrorClass::is_connection_lost`]) — what [`call`] redials on.
    connection_lost: bool,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
            connection_lost: false,
        }
    }

    fn from_rpc<E: RpcErrorClass + core::fmt::Display>(e: E) -> Self {
        Self {
            code: e.as_rpc_error().map(|r| r.code.clone()),
            message: e.to_string(),
            connection_lost: e.is_connection_lost(),
        }
    }

    /// The text of the MCP error content block.
    pub fn text(&self) -> String {
        match &self.code {
            Some(code) => format!("{code}: {}", self.message),
            None => self.message.clone(),
        }
    }
}

impl From<SessionUnavailable> for ToolError {
    fn from(why: SessionUnavailable) -> Self {
        match why {
            // public-text: cli-answer-strings-public
            SessionUnavailable::NotConnected => ToolError::new(
                "Not connected to a Fauna account. Ask the user for their Fauna handle (it is not a secret) and call connect with it.",
            ),
            SessionUnavailable::Pending { user_code } => ToolError::new(format!(
                "Waiting for the user to approve. They type {user_code} into Connect an app in one of their own Fauna apps, then approve the request."
            )),
            // public-text-end: cli-answer-strings-public
            SessionUnavailable::Failed(why) => ToolError::new(why),
            SessionUnavailable::AccountNeeded { held } => ToolError::new(format!(
                "Several Fauna accounts are connected ({}). Ask the user which one this is for and pass it as account.",
                held.join(", ")
            )),
            SessionUnavailable::UnknownAccount { account, held } => {
                ToolError::new(if held.is_empty() {
                    // public-text: cli-answer-strings-public
                    format!(
                        "{account} is not connected. Ask the user for their Fauna handle (it is not a secret) and call connect with it."
                    )
                    // public-text-end: cli-answer-strings-public
                } else {
                    format!(
                        "{account} is not connected; the connected accounts are {}. Call connect to add another.",
                        held.join(", ")
                    )
                })
            }
        }
    }
}

/// The answer to a write whose connection dropped under it: never retried,
/// because the nest may have done it before the socket went (a post created
/// twice is the dropped-command class's mirror image).
// public-text: cli-answer-strings-public
pub const WRITE_NOT_RETRIED: &str = "The connection to the nest dropped while this call was in flight. It was not retried, so it may or may not have taken effect: check (with a read tool) before calling it again.";
// public-text-end: cli-answer-strings-public

fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::new(format!("missing string argument `{name}`")))
}

fn opt_str(args: &Value, name: &str) -> Option<String> {
    args.get(name).and_then(Value::as_str).map(str::to_owned)
}

fn opt_i64(args: &Value, name: &str) -> Option<i64> {
    args.get(name).and_then(Value::as_i64)
}

fn data<T: Serialize>(reply: &T) -> Result<Value, ToolError> {
    serde_json::to_value(reply).map_err(|e| ToolError::new(format!("reply did not encode: {e}")))
}

/// A required actor id argument: 64 hex characters, the raw 32 bytes the wire
/// carries.
fn actor_arg(args: &Value) -> Result<Vec<u8>, ToolError> {
    fauna_core::hex32::decode(str_arg(args, "actor_id")?)
        .map(|id| id.to_vec())
        .map_err(|e| ToolError::new(format!("actor_id {e}")))
}

/// The optional supervised-admission pair of the three admission tools: the
/// guardian's actor id (64 hex) and the band (the shared wire token). The
/// nest's own refusals — a band without a guardian, a guardian on a
/// handle-less admission — come back verbatim.
fn supervision_args(args: &Value) -> Result<(Option<Vec<u8>>, Option<AgeBand>), ToolError> {
    let guardian = opt_str(args, "guardian_actor")
        .map(|hex| {
            fauna_core::hex32::decode(&hex)
                .map(|id| id.to_vec())
                .map_err(|e| ToolError::new(format!("guardian_actor {e}")))
        })
        .transpose()?;
    let band = opt_str(args, "age_band")
        .map(|token| {
            AgeBand::from_wire(&token).ok_or_else(|| {
                ToolError::new(format!(
                    "age_band must be one of {}",
                    AgeBand::ORDER.map(AgeBand::as_str).join(", ")
                ))
            })
        })
        .transpose()?;
    Ok((guardian, band))
}

fn i64_arg(args: &Value, name: &str) -> Result<i64, ToolError> {
    opt_i64(args, name).ok_or_else(|| ToolError::new(format!("missing integer argument `{name}`")))
}

/// The most of a stored report's decoded archive a get tool answers, in
/// bytes: far under the decoder's own bound, and more than any ordinary
/// report — a provider's aggregate report for a large domain may still pass
/// it, and the answer then says so.
pub const REPORT_RAW_TEXT_CAP: usize = 256 * 1024;

/// A mail-reports get tool's answer: the stored entry, and its gzipped archive
/// decoded with the apps' raw-view gunzip into text — lossily, since a failure
/// report is a whole mail as received — cut at [`REPORT_RAW_TEXT_CAP`] on a
/// character boundary.
fn report_with_raw<T: Serialize>(report: &T, archive_gz: &[u8]) -> Result<Value, ToolError> {
    let bytes = fauna_mail::dmarc_report::gunzip_xml(archive_gz)
        .map_err(|e| ToolError::new(format!("the stored report did not decode: {e}")))?;
    let mut raw = String::from_utf8_lossy(&bytes).into_owned();
    let truncated = raw.len() > REPORT_RAW_TEXT_CAP;
    if truncated {
        let mut cut = REPORT_RAW_TEXT_CAP;
        while !raw.is_char_boundary(cut) {
            cut -= 1;
        }
        raw.truncate(cut);
    }
    Ok(json!({
        "report": admin_data(report)?,
        "raw": raw,
        "raw_bytes": bytes.len(),
        "raw_truncated": truncated,
    }))
}

/// An admin reply as data, every 32-byte id in it as the hex the tools take
/// back — the wire's raw bytes would reach the model as a list of numbers.
fn admin_data<T: Serialize>(reply: &T) -> Result<Value, ToolError> {
    fn hex_ids(v: &mut Value) {
        match v {
            Value::Array(items) => {
                let bytes: Option<Vec<u8>> = (items.len() == 32)
                    .then(|| {
                        items
                            .iter()
                            .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                            .collect()
                    })
                    .flatten();
                match bytes {
                    Some(bytes) => {
                        let id: [u8; 32] = bytes.try_into().expect("32 bytes");
                        *v = Value::String(fauna_core::hex32::encode(&id));
                    }
                    None => items.iter_mut().for_each(hex_ids),
                }
            }
            Value::Object(fields) => fields.values_mut().for_each(hex_ids),
            _ => {}
        }
    }
    let mut v = data(reply)?;
    hex_ids(&mut v);
    Ok(v)
}

/// The users page's tools (`admin.md` § 2 Users), each one shared
/// `AdminClient` call. Deletion, eviction, handles, the admin roster and
/// approving a pending admin action are sovereign and offered by none.
async fn admin_users_call<R>(
    admin: AdminClient<R>,
    id: ToolId,
    args: &Value,
) -> Result<Value, ToolError>
where
    R: RpcRequester<Error: RpcErrorClass>,
{
    let ok = || Ok(json!({ "ok": true }));
    match id {
        ToolId::AdminUsersList => admin_data(
            &admin
                .users_list(opt_i64(args, "limit"), opt_i64(args, "offset").unwrap_or(0))
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminUsersGet => admin_data(
            &admin
                .users_get(actor_arg(args)?)
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminTiersList => {
            admin_data(&admin.tiers_list().await.map_err(ToolError::from_rpc)?)
        }
        ToolId::AdminUsersAdmit => {
            let (guardian, band) = supervision_args(args)?;
            admin
                .users_create(
                    actor_arg(args)?,
                    str_arg(args, "tier")?,
                    opt_str(args, "label").unwrap_or_default(),
                    opt_str(args, "handle"),
                    guardian,
                    band,
                )
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminUsersUpdate => {
            let actor = actor_arg(args)?;
            // The wire sets the tier and the label together; an omitted label
            // keeps the member's current one rather than blanking it.
            let label = match opt_str(args, "label") {
                Some(label) => label,
                None => {
                    admin
                        .users_get(actor.clone())
                        .await
                        .map_err(ToolError::from_rpc)?
                        .user
                        .label
                }
            };
            admin
                .users_update(actor, str_arg(args, "tier")?, label)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminUsersSuspend => admin_data(
            &admin
                .users_suspend(
                    actor_arg(args)?,
                    opt_str(args, "reason").unwrap_or_default(),
                    String::new(),
                )
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminUsersReinstate => {
            admin
                .users_cancel_eviction(actor_arg(args)?)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminInviteCodesList => admin_data(
            &admin
                .invite_codes_list()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        // An empty code asks the nest to mint one (`admin.md` § 2 → Section 4).
        ToolId::AdminInviteCodesCreate => {
            let (guardian, band) = supervision_args(args)?;
            admin_data(
                &admin
                    .invite_codes_create(
                        "",
                        str_arg(args, "tier")?,
                        i64_arg(args, "uses")?,
                        guardian,
                        band,
                    )
                    .await
                    .map_err(ToolError::from_rpc)?,
            )
        }
        ToolId::AdminInviteCodesDelete => {
            admin
                .invite_codes_delete(str_arg(args, "code")?)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminInviteRequestsList => admin_data(
            &admin
                .invite_requests_list()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminInviteRequestsApprove => {
            let (guardian, band) = supervision_args(args)?;
            admin_data(
                &admin
                    .invite_requests_approve(
                        i64_arg(args, "id")?,
                        opt_str(args, "tier"),
                        opt_str(args, "label"),
                        guardian,
                        band,
                    )
                    .await
                    .map_err(ToolError::from_rpc)?,
            )
        }
        ToolId::AdminInviteRequestsDeny => {
            admin
                .invite_requests_deny(i64_arg(args, "id")?, opt_str(args, "reason"))
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminRegistrationModeSet => {
            let mode =
                RegistrationMode::from_wire_str(str_arg(args, "mode")?).ok_or_else(|| {
                    ToolError::new("mode must be one of open, invite_required, closed")
                })?;
            let max_free_users = match args.get("max_free_users") {
                None => {
                    return Err(ToolError::new(
                        "missing argument `max_free_users` (null for no cap)",
                    ));
                }
                Some(Value::Null) => None,
                Some(v) => Some(v.as_u64().ok_or_else(|| {
                    ToolError::new("max_free_users must be a non-negative integer or null")
                })?),
            };
            admin
                .set_registration_mode(mode, max_free_users)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        other => unreachable!("{other:?} is not a users-page tool"),
    }
}

/// An actor id as the hex the tools take back.
fn hex_id(id: &[u8]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// A required boolean argument.
fn bool_arg(args: &Value, name: &str) -> Result<bool, ToolError> {
    args.get(name)
        .and_then(Value::as_bool)
        .ok_or_else(|| ToolError::new(format!("missing boolean argument `{name}`")))
}

/// A required argument that may be `null` — the absence a caller states,
/// never a default (the region withdrawn, the apex cleared).
fn nullable_str<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>, ToolError> {
    match args.get(name) {
        None => Err(ToolError::new(format!(
            "missing argument `{name}` (null to clear)"
        ))),
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(ToolError::new(format!("{name} must be a string or null"))),
    }
}

/// A tier's five quotas, as the create and update kinds both take them.
fn tier_caps(args: &Value) -> Result<(String, [i64; 5]), ToolError> {
    Ok((
        str_arg(args, "name")?.to_owned(),
        [
            i64_arg(args, "max_inbox_bytes")?,
            i64_arg(args, "max_storage_bytes")?,
            i64_arg(args, "max_devices")?,
            i64_arg(args, "max_blob_size")?,
            i64_arg(args, "max_feeds")?,
        ],
    ))
}

/// The nest-side admin pages' tools — settings ("Tiers"), nest, logs, web and
/// DNS (`admin.md` § 3, § N, § 7, § The admin audit log; `dns-management.md`),
/// each one shared client call or one typed request. The pages' sovereign
/// acts — the factory reset, the deployment seed, key rotations, the legal
/// takedown, the host address and certificate writes — are offered by none.
async fn admin_nest_side_call<R>(rpc: R, id: ToolId, args: &Value) -> Result<Value, ToolError>
where
    R: RpcRequester<Error: RpcErrorClass>,
{
    let ok = || Ok(json!({ "ok": true }));
    // Each arm takes the one session, so the admin client is built per arm.
    macro_rules! admin {
        () => {
            AdminClient::new(rpc)
        };
    }
    match id {
        ToolId::AdminTierCreate => {
            let (name, [inbox, storage, devices, blob, feeds]) = tier_caps(args)?;
            admin!()
                .tiers_create(AdminTierCreateRequest {
                    name,
                    max_inbox_bytes: inbox,
                    max_storage_bytes: storage,
                    max_devices: devices,
                    max_blob_size: blob,
                    max_feeds: feeds,
                    ..Default::default()
                })
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminTierUpdate => {
            let (name, [inbox, storage, devices, blob, feeds]) = tier_caps(args)?;
            admin!()
                .tiers_update(AdminTierUpdateRequest {
                    name,
                    max_inbox_bytes: inbox,
                    max_storage_bytes: storage,
                    max_devices: devices,
                    max_blob_size: blob,
                    max_feeds: feeds,
                    ..Default::default()
                })
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminTierDelete => {
            admin!()
                .tiers_delete(str_arg(args, "name")?)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminMembershipTiersList => admin_data(
            &admin!()
                .membership_tiers_list()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminMembershipTierSet => {
            admin!()
                .membership_tiers_set(AdminMembershipTierSetRequest {
                    tier_name: str_arg(args, "tier_name")?.to_owned(),
                    admin_tier: str_arg(args, "admin_tier")?.to_owned(),
                    lapse_tier: opt_str(args, "lapse_tier"),
                    ..Default::default()
                })
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminMembershipTierClear => {
            admin!()
                .membership_tiers_clear(str_arg(args, "tier_name")?.to_owned())
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminRegionGet => data(
            &admin!()
                .region_status()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminRegionSet => {
            let region = nullable_str(args, "region")?
                .map(RegionCode::parse)
                .transpose()
                .map_err(|e| ToolError::new(format!("region {e}")))?;
            admin!()
                .set_region(region)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminWebAppOriginGet => data(
            &admin!()
                .web_app_origin()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminWebAppOriginSet => {
            let mode = WebAppOrigin::parse(str_arg(args, "mode")?)
                .ok_or_else(|| ToolError::new("mode must be bundled or central"))?;
            data(
                &admin!()
                    .set_web_app_origin(mode)
                    .await
                    .map_err(ToolError::from_rpc)?,
            )
        }
        ToolId::AdminUpdateChannelGet => data(
            &admin!()
                .update_channel()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminUpdateChannelSet => {
            let channel = UpdateChannel::from_id(str_arg(args, "channel")?)
                .ok_or_else(|| ToolError::new("channel must be one of stable, test, dev"))?;
            admin!()
                .set_update_channel(channel)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminServicesList => data(
            &admin!()
                .services_list()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminServiceSet => data(
            &admin!()
                .services_update(str_arg(args, "name")?, bool_arg(args, "enabled")?)
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminServingPortSet => {
            let port = u16::try_from(i64_arg(args, "port")?)
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| ToolError::new("port must be 1–65535"))?;
            admin!()
                .set_serving_port(port)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminHostRestart => {
            admin!()
                .request_host_restart()
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminSigningKeys => data(
            &admin!()
                .issuer_key_status()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminReportsList => admin_data(
            &admin!()
                .abuse_report_queue()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminReportResolve => {
            let outcome = match str_arg(args, "outcome")? {
                "acted" => AbuseReportOutcome::Acted,
                "dismissed" => AbuseReportOutcome::Dismissed,
                _ => return Err(ToolError::new("outcome must be acted or dismissed")),
            };
            data(
                &admin!()
                    .abuse_report_resolve(str_arg(args, "report_id")?, outcome)
                    .await
                    .map_err(ToolError::from_rpc)?,
            )
        }
        ToolId::AdminBlockedList => admin_data(
            &ModerationClient::new(rpc)
                .admin_block_list()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminReportBlock => data(
            &ModerationClient::new(rpc)
                .admin_block(
                    str_arg(args, "report_id")?,
                    args.get("restore")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                )
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminLogs => data(&admin!().logs().await.map_err(ToolError::from_rpc)?),
        ToolId::AdminAuditList => admin_data(
            &admin!()
                .audit_list(opt_i64(args, "limit"), opt_i64(args, "before_id"))
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminAuditIntegrity => admin_data(
            &admin!()
                .audit_integrity()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminApexActorGet => {
            let actor = WebClient::new(rpc)
                .get_apex_actor()
                .await
                .map_err(ToolError::from_rpc)?;
            Ok(json!({ "actor_id": actor.as_deref().map(hex_id) }))
        }
        ToolId::AdminApexActorSet => {
            let actor = nullable_str(args, "actor_id")?
                .map(|id| {
                    fauna_core::hex32::decode(id)
                        .map(|id| id.to_vec())
                        .map_err(|e| ToolError::new(format!("actor_id {e}")))
                })
                .transpose()?;
            let actor = WebClient::new(rpc)
                .set_apex_actor(actor)
                .await
                .map_err(ToolError::from_rpc)?;
            Ok(json!({ "actor_id": actor.as_deref().map(hex_id) }))
        }
        ToolId::AdminDnsRecords => {
            let reply: ListRecordsReply = rpc
                .request(
                    "fauna.dns.list_records",
                    ListRecordsRequest {
                        domain: opt_str(args, "domain"),
                        ..Default::default()
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            data(&reply)
        }
        ToolId::AdminDnsVerify => {
            let reply: VerifyRecordsReply = rpc
                .request(
                    "fauna.dns.verify_records",
                    VerifyRecordsRequest {
                        domain: opt_str(args, "domain"),
                        ..Default::default()
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            data(&reply)
        }
        ToolId::AdminCertStatus => {
            let domains = args
                .get("domains")
                .and_then(Value::as_array)
                .filter(|d| !d.is_empty())
                .and_then(|d| {
                    d.iter()
                        .map(|v| v.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| ToolError::new("domains must be a non-empty list of names"))?;
            let reply: CertStatusReply = rpc
                .request(
                    "fauna.tls.cert_status",
                    CertStatusRequest {
                        domains,
                        ..Default::default()
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            data(&reply)
        }
        ToolId::AdminDomainsList => {
            let reply: ListLocalDomainsReply = rpc
                .request(
                    "fauna.bridges.list_local_domains",
                    ListLocalDomainsRequest {},
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        ToolId::AdminDomainRemove => {
            let reply: RemoveLocalDomainReply = rpc
                .request(
                    "fauna.bridges.remove_local_domain",
                    RemoveLocalDomainRequest {
                        domain: str_arg(args, "domain")?.to_owned(),
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        ToolId::AdminDomainRestore => {
            let reply: RestoreLocalDomainReply = rpc
                .request(
                    "fauna.bridges.restore_local_domain",
                    RestoreLocalDomainRequest {
                        domain: str_arg(args, "domain")?.to_owned(),
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        ToolId::AdminFeatureLimitsGet => admin_data(
            &FeaturesClient::new(rpc)
                .authored(AuthoringTier::Admin)
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminFeatureLimitSet => {
            let feature = GatedFeature::from_key(str_arg(args, "feature")?).ok_or_else(|| {
                ToolError::new("feature is not a gated feature this server knows")
            })?;
            let policy = match args.get("policy") {
                None => {
                    return Err(ToolError::new(
                        "missing argument `policy` (null to withdraw)",
                    ));
                }
                Some(Value::Null) => None,
                Some(v) => Some(
                    serde_json::from_value::<FeaturePolicy>(v.clone())
                        .map_err(|e| ToolError::new(format!("policy: {e}")))?,
                ),
            };
            FeaturesClient::new(rpc)
                .author(AuthoringTier::Admin, feature, policy)
                .await
                .map_err(|e| match e {
                    SaveError::Invalid(reason) => ToolError::new(format!("{reason:?}")),
                    SaveError::Rpc(e) => ToolError::from_rpc(e),
                })?;
            ok()
        }
        ToolId::AdminContentRulesGet => {
            let reply = admin!()
                .content_policy()
                .await
                .map_err(ToolError::from_rpc)?;
            match reply.policy {
                None => Ok(json!({ "document": null })),
                Some(signed) => {
                    let document = decode_document(&signed.document).map_err(|e| {
                        ToolError::new(format!("the stored rules did not decode: {e:?}"))
                    })?;
                    Ok(json!({
                        "document": data(&document)?,
                        "sequence": signed.sequence,
                        "issued_at": signed.issued_at,
                    }))
                }
            }
        }
        ToolId::AdminContentRulesSet => {
            let document = serde_json::from_value::<ContentPolicyDocument>(
                args.get("document")
                    .cloned()
                    .ok_or_else(|| ToolError::new("missing argument `document`"))?,
            )
            .map_err(|e| ToolError::new(format!("document: {e}")))?;
            let reply = admin!()
                .set_content_policy(document)
                .await
                .map_err(ToolError::from_rpc)?;
            match reply.defect {
                Some(defect) => Err(ToolError::new(format!("the rules were refused: {defect}"))),
                None => Ok(json!({
                    "ok": true,
                    "sequence": reply.policy.as_ref().map(|p| p.sequence),
                })),
            }
        }
        ToolId::AdminTransportGet => data(
            &admin!()
                .transport_policy()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminTransportSet => {
            let max = match args.get("max_conns_per_ip") {
                None => {
                    return Err(ToolError::new(
                        "missing argument `max_conns_per_ip` (null for the default)",
                    ));
                }
                Some(Value::Null) => None,
                Some(v) => Some(
                    v.as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| *n != 0)
                        .ok_or_else(|| {
                            ToolError::new("max_conns_per_ip must be a positive integer or null")
                        })?,
                ),
            };
            admin!()
                .set_transport_policy(max)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminSubhandlesSet => {
            admin!()
                .set_subhandles(bool_arg(args, "enabled")?)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminStorageCapSet => {
            let max_bytes = match args.get("max_bytes") {
                None => {
                    return Err(ToolError::new(
                        "missing argument `max_bytes` (null to remove the cap)",
                    ));
                }
                Some(Value::Null) => None,
                Some(v) => Some(v.as_u64().ok_or_else(|| {
                    ToolError::new("max_bytes must be a non-negative integer or null")
                })?),
            };
            admin!()
                .set_max_storage_bytes(max_bytes)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminCorsSet => {
            let origins = args
                .get("origins")
                .and_then(Value::as_array)
                .and_then(|o| {
                    o.iter()
                        .map(|v| v.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| ToolError::new("origins must be a list of strings"))?;
            admin!()
                .set_cors_origins(origins)
                .await
                .map_err(ToolError::from_rpc)?;
            ok()
        }
        ToolId::AdminGc => data(
            &admin!()
                .gc(
                    opt_i64(args, "grace_period_secs"),
                    args.get("dry_run")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                )
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminWorkerStatus => data(
            &admin!()
                .worker_status()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminClusterStatus => data(
            &admin!()
                .cluster_status()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::AdminDomainAdd => {
            // The first active domain becomes the primary domain, never removable
            // (`admin-dns-add-domain-primary-warning`): that choice is the
            // admin's, made in their own app, so this tool only adds an
            // additional one.
            let listed: ListLocalDomainsReply = rpc
                .request(
                    "fauna.bridges.list_local_domains",
                    ListLocalDomainsRequest {},
                )
                .await
                .map_err(ToolError::from_rpc)?;
            if listed.active.is_empty() {
                // public-text: cli-answer-strings-public
                return Err(ToolError::new(
                    "this server has no active domain yet, and the first domain added becomes its permanent main domain: the admin adds that one in their own Fauna app",
                ));
                // public-text-end: cli-answer-strings-public
            }
            let reply: AddLocalDomainReply = rpc
                .request(
                    "fauna.bridges.add_local_domain",
                    AddLocalDomainRequest {
                        domain: str_arg(args, "domain")?.to_owned(),
                        mta_sts_cert_mode: DEFAULT_CERT_MODE.to_owned(),
                        ..Default::default()
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        ToolId::AdminDomainConfigSet => {
            let dmarc_policy_mode = opt_str(args, "dmarc_policy_mode")
                .map(|m| {
                    serde_json::from_value::<DmarcMode>(Value::String(m)).map_err(|_| {
                        ToolError::new("dmarc_policy_mode must be none, quarantine or reject")
                    })
                })
                .transpose()?;
            let reply: UpdateLocalDomainConfigReply = rpc
                .request(
                    "fauna.bridges.update_local_domain_config",
                    UpdateLocalDomainConfigRequest {
                        domain: str_arg(args, "domain")?.to_owned(),
                        mta_sts_max_age_seconds: opt_i64(args, "mta_sts_max_age_seconds"),
                        mta_sts_cert_mode: opt_str(args, "mta_sts_cert_mode"),
                        spf_record: opt_str(args, "spf_record"),
                        dmarc_policy_mode,
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        ToolId::AdminCatchAllSet => {
            let reply: SetCatchAllActorReply = rpc
                .request(
                    "fauna.bridges.set_catch_all_actor",
                    SetCatchAllActorRequest {
                        domain: str_arg(args, "domain")?.to_owned(),
                        actor_id: nullable_actor(args)?,
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        ToolId::AdminRoleAddressSet => {
            let role = serde_json::from_value::<RoleAddressKind>(Value::String(
                str_arg(args, "role")?.to_owned(),
            ))
            .map_err(|_| ToolError::new("role must be postmaster, abuse, noc or security"))?;
            let reply: SetRoleAddressReply = rpc
                .request(
                    "fauna.bridges.set_role_address",
                    SetRoleAddressRequest {
                        domain: str_arg(args, "domain")?.to_owned(),
                        role,
                        actor_id: nullable_actor(args)?,
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        ToolId::AdminDkimIntervalSet => {
            let rotation_days =
                match args.get("rotation_days") {
                    None => {
                        return Err(ToolError::new(
                            "missing argument `rotation_days` (null for the server's default)",
                        ));
                    }
                    Some(Value::Null) => None,
                    Some(v) => Some(v.as_i64().ok_or_else(|| {
                        ToolError::new("rotation_days must be an integer or null")
                    })?),
                };
            let reply: SetDkimRotationDaysReply = rpc
                .request(
                    "fauna.bridges.set_dkim_rotation_days",
                    SetDkimRotationDaysRequest {
                        domain: str_arg(args, "domain")?.to_owned(),
                        rotation_days,
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        ToolId::AdminDomainRenameStatus => {
            let reply: GetPrimaryDomainRenameStatusReply = rpc
                .request(
                    "fauna.bridges.get_primary_domain_rename_status",
                    GetPrimaryDomainRenameStatusRequest {},
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        ToolId::AdminDomainRenamesList => {
            let reply: ListPrimaryDomainRenamesReply = rpc
                .request(
                    "fauna.bridges.list_primary_domain_renames",
                    ListPrimaryDomainRenamesRequest {},
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        ToolId::AdminDkimSelectorsList => {
            let reply: ListDkimSelectorsReply = rpc
                .request(
                    "fauna.bridges.list_dkim_selectors",
                    ListDkimSelectorsRequest {
                        domain: opt_str(args, "domain"),
                        extra: Default::default(),
                    },
                )
                .await
                .map_err(ToolError::from_rpc)?;
            admin_data(&reply)
        }
        other => unreachable!("{other:?} is not a nest-side admin tool"),
    }
}

/// The required `actor_id` argument that may be `null` — a member's actor id
/// (64 hex) to designate, or the stated absence that clears it.
fn nullable_actor(args: &Value) -> Result<Option<ByteBuf>, ToolError> {
    nullable_str(args, "actor_id")?
        .map(|id| {
            fauna_core::hex32::decode(id)
                .map(|id| ByteBuf::from(id.to_vec()))
                .map_err(|e| ToolError::new(format!("actor_id {e}")))
        })
        .transpose()
}

/// The report pages' window: `window_days` (30 when omitted, the page's own
/// default) and an optional one domain.
fn report_window(args: &Value) -> Result<(u32, Option<String>), ToolError> {
    let days = match args.get("window_days") {
        None => 30,
        Some(v) => v
            .as_u64()
            .and_then(|d| u32::try_from(d).ok())
            .filter(|d| (1..=366).contains(d))
            .ok_or_else(|| ToolError::new("window_days must be between 1 and 366"))?,
    };
    Ok((days, opt_str(args, "domain")))
}

/// One mail policy section saved from the effective policy with `changes`
/// laid over it: the wire's `put_<section>_policy` is a full PUT (a field
/// left `None` falls back to the catalog default), so a save that sent only
/// the changed fields would silently reset every other one. A field name the
/// effective policy does not carry is refused, never forwarded.
fn merged_policy<P, Q>(current: &P, changes: &Value) -> Result<Q, ToolError>
where
    P: Serialize,
    Q: serde::de::DeserializeOwned,
{
    let Some(changes) = changes.as_object() else {
        return Err(ToolError::new(
            "values must be an object of fields to change",
        ));
    };
    let mut merged = data(current)?;
    let fields = merged
        .as_object_mut()
        .ok_or_else(|| ToolError::new("the current policy did not encode as an object"))?;
    // A newer peer's unknown keys ride the reply's catch-all; a save sends
    // only the fields this build knows.
    fields.remove("extra");
    for (name, value) in changes {
        if name == "extra" || !fields.contains_key(name) {
            return Err(ToolError::new(format!(
                "`{name}` is not a field of this policy (admin_mail_settings names them)"
            )));
        }
        fields.insert(name.clone(), value.clone());
    }
    serde_json::from_value(merged).map_err(|e| ToolError::new(format!("values: {e}")))
}

/// The mail-side admin pages' tools (`admin.md` § 6 Mail, § Mail reports,
/// § 4 Aliases, § 8 Calendar, § Contacts, § Files, the bridges roster), each
/// one shared `MailAdminClient` call or a fold of a page's reads. The secret
/// rotations, the Microsoft registration and the pending-bridge roster's
/// writes are sovereign and offered by none.
async fn admin_mail_call<R>(
    mail: MailAdminClient<R>,
    id: ToolId,
    args: &Value,
) -> Result<Value, ToolError>
where
    R: RpcRequester<Error: RpcErrorClass>,
{
    let ok = || Ok(json!({ "ok": true }));
    let rpc = ToolError::from_rpc;
    match id {
        // The mail page's form reads, as its machine hydrates them: the
        // effective config and the two policies that live beside it.
        ToolId::AdminMailSettings => {
            let config = mail.get_mail_config().await.map_err(rpc)?;
            let alias = mail.get_alias_policy().await.map_err(rpc)?;
            let dmarc = mail.get_dmarc_policy().await.map_err(rpc)?;
            let mut out = admin_data(&config)?;
            out["alias_policy"] = data(&alias)?;
            out["dmarc_policy"] = data(&dmarc)?;
            Ok(out)
        }
        ToolId::AdminMailHealth => {
            let health = mail.mail_health().await.map_err(rpc)?;
            let warmup = mail.outbound_warmup_status().await.map_err(rpc)?;
            let baseline = mail.get_spam_baseline_state().await.map_err(rpc)?;
            Ok(json!({
                "health": admin_data(&health)?,
                "warmup": data(&warmup)?,
                "spam_baseline": data(&baseline)?,
            }))
        }
        ToolId::AdminMailEnabledSet => {
            mail.set_mail_enabled(bool_arg(args, "enabled")?)
                .await
                .map_err(rpc)?;
            ok()
        }
        ToolId::AdminMailAutoEnableSet => {
            mail.set_auto_enable_mail_for_new_users(bool_arg(args, "enabled")?)
                .await
                .map_err(rpc)?;
            ok()
        }
        ToolId::AdminMailPolicySet => {
            let changes = args.get("values").unwrap_or(&Value::Null);
            match str_arg(args, "section")? {
                "alias" => {
                    let current = mail.get_alias_policy().await.map_err(rpc)?;
                    mail.put_alias_policy(merged_policy(&current, changes)?)
                        .await
                        .map_err(rpc)?;
                }
                "dmarc" => {
                    let current = mail.get_dmarc_policy().await.map_err(rpc)?;
                    mail.put_dmarc_policy(merged_policy(&current, changes)?)
                        .await
                        .map_err(rpc)?;
                }
                section @ ("spam" | "auth" | "submission" | "imap" | "outbound") => {
                    let config = mail.get_mail_config().await.map_err(rpc)?;
                    match section {
                        "spam" => {
                            mail.put_spam_policy(merged_policy(&config.spam, changes)?)
                                .await
                        }
                        "auth" => {
                            mail.put_auth_policy(merged_policy(&config.auth, changes)?)
                                .await
                        }
                        "submission" => {
                            mail.put_submission_policy(merged_policy(&config.submission, changes)?)
                                .await
                        }
                        "imap" => {
                            mail.put_imap_policy(merged_policy(&config.imap, changes)?)
                                .await
                        }
                        _ => {
                            mail.put_outbound_policy(merged_policy(&config.outbound, changes)?)
                                .await
                        }
                    }
                    .map_err(rpc)?;
                }
                other => {
                    return Err(ToolError::new(format!(
                        "section must be one of {}, not {other}",
                        catalog::MAIL_POLICY_SECTIONS.join(", ")
                    )));
                }
            }
            ok()
        }
        ToolId::AdminMailDeliverabilityCheck => {
            admin_data(&mail.run_deliverability_diagnostics().await.map_err(rpc)?)
        }
        ToolId::AdminMailBlocklistCheck => {
            admin_data(&mail.blocklist_self_check_run().await.map_err(rpc)?)
        }
        ToolId::AdminMailWarmupReset => data(&mail.outbound_warmup_reset().await.map_err(rpc)?),
        ToolId::AdminMailSpamBaselinePublish => {
            data(&mail.publish_spam_baseline().await.map_err(rpc)?)
        }
        ToolId::AdminMailReportsDmarc => {
            let (days, domain) = report_window(args)?;
            admin_data(
                &mail
                    .list_dmarc_aggregate_reports(days, domain)
                    .await
                    .map_err(rpc)?,
            )
        }
        ToolId::AdminMailReportsForensic => {
            let (days, domain) = report_window(args)?;
            admin_data(
                &mail
                    .list_dmarc_forensic_reports(days, domain)
                    .await
                    .map_err(rpc)?,
            )
        }
        ToolId::AdminMailReportsTls => {
            let (days, domain) = report_window(args)?;
            admin_data(
                &mail
                    .list_tlsrpt_inbound_reports(days, domain)
                    .await
                    .map_err(rpc)?,
            )
        }
        ToolId::AdminMailReportsDmarcGet => {
            let reply = mail
                .get_dmarc_aggregate_report(
                    str_arg(args, "our_domain")?.to_owned(),
                    str_arg(args, "submitter")?.to_owned(),
                    str_arg(args, "report_id")?.to_owned(),
                )
                .await
                .map_err(rpc)?;
            report_with_raw(&reply.report, &reply.payload_xml_gz)
        }
        ToolId::AdminMailReportsForensicGet => {
            let reply = mail
                .get_dmarc_forensic_report(i64_arg(args, "id")?)
                .await
                .map_err(rpc)?;
            report_with_raw(&reply.report, &reply.payload_raw_gz)
        }
        ToolId::AdminMailReportsTlsGet => {
            let reply = mail
                .get_tlsrpt_inbound_report(
                    str_arg(args, "our_domain")?.to_owned(),
                    str_arg(args, "submitter")?.to_owned(),
                    str_arg(args, "report_id")?.to_owned(),
                )
                .await
                .map_err(rpc)?;
            report_with_raw(&reply.report, &reply.payload_json_gz)
        }
        ToolId::AdminMailReportsBlocklistHistory => admin_data(
            &mail
                .list_blocklist_self_check_history(opt_i64(args, "window_days").unwrap_or(0))
                .await
                .map_err(rpc)?,
        ),
        ToolId::AdminMailReportsDiagnosticRuns => admin_data(
            &mail
                .list_deliverability_diagnostic_runs(opt_i64(args, "limit").unwrap_or(0))
                .await
                .map_err(rpc)?,
        ),
        ToolId::AdminForwardersList => {
            admin_data(&forwarders_hex(mail.list_forwarders().await.map_err(rpc)?)?)
        }
        ToolId::AdminForwardersCreate => {
            let reply = mail
                .create_forwarder(
                    str_arg(args, "local_domain")?,
                    str_arg(args, "pattern")?,
                    str_arg(args, "forward_target")?,
                )
                .await
                .map_err(rpc)?;
            Ok(json!({ "alias_id": hex_id(&reply.alias_id) }))
        }
        ToolId::AdminForwardersDelete => {
            let id = decode_hex(str_arg(args, "alias_id")?)
                .ok_or_else(|| ToolError::new("alias_id must be hex"))?;
            data(&mail.delete_forwarder(id).await.map_err(rpc)?)
        }
        ToolId::AdminCalendarPortGet => {
            Ok(json!({ "port": mail.get_caldav_port().await.map_err(rpc)? }))
        }
        ToolId::AdminCalendarEnabledSet => {
            mail.set_caldav_enabled(bool_arg(args, "enabled")?)
                .await
                .map_err(rpc)?;
            ok()
        }
        ToolId::AdminCalendarPortSet => {
            let port = args
                .get("port")
                .and_then(Value::as_u64)
                .and_then(|p| u16::try_from(p).ok())
                .filter(|p| *p != 0)
                .ok_or_else(|| ToolError::new("port must be between 1 and 65535"))?;
            mail.set_caldav_port(port).await.map_err(rpc)?;
            ok()
        }
        ToolId::AdminContactsEnabledSet => {
            mail.set_carddav_enabled(bool_arg(args, "enabled")?)
                .await
                .map_err(rpc)?;
            ok()
        }
        ToolId::AdminFilesEnabledSet => {
            mail.set_webdav_enabled(bool_arg(args, "enabled")?)
                .await
                .map_err(rpc)?;
            ok()
        }
        // Every key on the rosters is 32 bytes, so `admin_data` answers each
        // as the hex the human finds the card by and the prompt names.
        ToolId::AdminBridgesPendingList => {
            admin_data(&mail.list_pending_bridges().await.map_err(rpc)?)
        }
        ToolId::AdminBridgesApprovedList => {
            admin_data(&mail.list_service_users(None, None).await.map_err(rpc)?)
        }
        other => unreachable!("{other:?} is not a mail-side admin tool"),
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    (s.len().is_multiple_of(2) && !s.is_empty())
        .then(|| {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
                .collect()
        })
        .flatten()
}

/// The forwarder list with each 16-byte id as the hex `admin_forwarders_delete`
/// takes back ([`admin_data`] turns only 32-byte ids to hex).
fn forwarders_hex(reply: ListForwardersReply) -> Result<Value, ToolError> {
    let mut v = data(&reply)?;
    if let Some(rows) = v.get_mut("forwarders").and_then(Value::as_array_mut) {
        for (row, f) in rows.iter_mut().zip(&reply.forwarders) {
            row["alias_id"] = json!(hex_id(&f.alias_id));
        }
    }
    Ok(v)
}

/// Run `tool` with `args` against `host`; the reply as JSON data.
///
/// **A dropped session** (`apps/cli.md` § The device shell → *A dropped
/// session*): when the connection goes away under the call, the session is
/// redialled once; a read-only tool is then run again on the new session, a
/// write is answered [`WRITE_NOT_RETRIED`] — the catalog's `read_only`
/// annotation decides, never `idempotent` (an idempotent write is still a
/// write the nest may already have applied).
pub async fn call<H: Host>(host: &H, tool: &Tool, args: &Value) -> Result<Value, ToolError> {
    if tool.id == ToolId::Connect {
        let admin = args
            .get("admin")
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| ToolError::new("admin must be a boolean"))
            })
            .transpose()?
            .unwrap_or(false);
        let started = host.connect(str_arg(args, "handle")?, admin).await?;
        let mut answer = data(&started)?;
        // public-text: cli-answer-strings-public
        let mut for_the_user = format!(
            "Type {} into Connect an app in one of your Fauna apps, then approve the request.",
            started.user_code
        );
        // public-text-end: cli-answer-strings-public
        if let Some(custody) = started.custody {
            for_the_user = format!("{for_the_user} {custody}");
        }
        answer["for_the_user"] = json!(for_the_user);
        return Ok(answer);
    }
    let account = account_arg(args)?;
    if tool.id == ToolId::AdminRegistrationModeGet {
        // Public: read on the shell's own anonymous connection, not through
        // the principal session (`Host::setup_status`).
        let status = host.setup_status(account).await?;
        return Ok(json!({
            "registration_mode": status.registration_mode,
            "max_free_users": status.max_free_users,
        }));
    }
    if let Some(op) = conversation_op(tool.id, args)? {
        // The door, not the nest: a fresh connection per operation, so there
        // is no session to redial.
        return host.conversation(account, op).await;
    }
    let rpc = host.session(account).await?;
    let reply = match dispatch(rpc.clone(), tool, args).await {
        Err(e) if e.connection_lost => {
            let fresh = host.redial(account, &rpc).await;
            if !tool.annotations.read_only {
                return Err(ToolError::new(WRITE_NOT_RETRIED));
            }
            dispatch(fresh?, tool, args).await
        }
        reply => reply,
    };
    if tool.id == ToolId::AdminMailSettings {
        // The mail page's two settings that read back only through the
        // public setup status — no admin string's kind — read as the
        // registration posture is (`Host::setup_status`).
        let mut settings = reply?;
        let status = host.setup_status(account).await?;
        settings["auto_enable_mail_for_new_users"] = json!(status.auto_enable_mail_for_new_users);
        settings["microsoft_registration"] = data(&status.microsoft_registration)?;
        return Ok(settings);
    }
    reply
}

/// The oracle operation a conversations tool asks for; `None` for every other
/// tool.
fn conversation_op(id: ToolId, args: &Value) -> Result<Option<ConversationOp>, ToolError> {
    Ok(Some(match id {
        ToolId::ConversationsList => ConversationOp::ListRooms,
        ToolId::ConversationsRead => ConversationOp::ReadRoom {
            room_id: str_arg(args, "room_id")?.to_owned(),
            before: match args.get("before") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_u64().ok_or_else(|| {
                    ToolError::new("before must be a non-negative integer (a page's earlier)")
                })?),
            },
        },
        ToolId::ConversationsSearch => ConversationOp::Search {
            query: str_arg(args, "query")?.to_owned(),
            room_id: opt_str(args, "room_id"),
        },
        ToolId::ConversationsSend => ConversationOp::Send {
            room_id: str_arg(args, "room_id")?.to_owned(),
            body: str_arg(args, "text")?.to_owned(),
        },
        _ => return Ok(None),
    }))
}

/// The optional `account` argument every tool but `connect` takes.
fn account_arg(args: &Value) -> Result<Option<&str>, ToolError> {
    match args.get(catalog::ACCOUNT_ARG) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(ToolError::new(
            "account must be a string (an account's handle)",
        )),
    }
}

/// One tool's call over `rpc`.
async fn dispatch<R>(rpc: R, tool: &Tool, args: &Value) -> Result<Value, ToolError>
where
    R: RpcRequester<Error: RpcErrorClass>,
{
    match tool.id {
        ToolId::Connect
        | ToolId::AdminRegistrationModeGet
        | ToolId::ConversationsList
        | ToolId::ConversationsRead
        | ToolId::ConversationsSearch
        | ToolId::ConversationsSend => {
            unreachable!("answered by `call`")
        }
        ToolId::FeedList => data(
            &FeedClient::new(rpc)
                .feed_list()
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::FeedGet => data(
            &FeedClient::new(rpc)
                .feed_get(str_arg(args, "feed_id")?)
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::FeedPosts => data(
            &FeedClient::new(rpc)
                .feed_posts(
                    str_arg(args, "feed_id")?,
                    opt_i64(args, "cursor"),
                    opt_i64(args, "limit"),
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::FeedLocal => data(
            &FeedClient::new(rpc)
                .feed_local_posts(
                    opt_i64(args, "cursor"),
                    opt_i64(args, "limit"),
                    opt_str(args, "search"),
                )
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::FeedTrending => data(
            &FeedClient::new(rpc)
                .feed_trending_posts(opt_i64(args, "limit"), None, None, opt_str(args, "search"))
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        ToolId::PostCreate => {
            let request = PostAuthorCreateRequest {
                body: PostBody::Text {
                    content: str_arg(args, "text")?.to_owned(),
                    facets: vec![],
                },
                references: vec![],
                content_warning: opt_str(args, "content_warning"),
                extra: Default::default(),
            };
            let reply: PostAuthorCreateReply = rpc
                .request(KIND_CREATE, request)
                .await
                .map_err(ToolError::from_rpc)?;
            data(&reply)
        }
        ToolId::AdminDashboard => {
            // The dashboard's two reads, as the page's cards fold them
            // (`admin.md` § 1 Dashboard): the counts and the running version.
            let admin = AdminClient::new(rpc);
            let stats = admin.stats().await.map_err(ToolError::from_rpc)?;
            let status = admin.status().await.map_err(ToolError::from_rpc)?;
            Ok(json!({ "stats": data(&stats)?, "status": data(&status)? }))
        }
        id @ (ToolId::AdminUsersList
        | ToolId::AdminUsersGet
        | ToolId::AdminTiersList
        | ToolId::AdminUsersAdmit
        | ToolId::AdminUsersUpdate
        | ToolId::AdminUsersSuspend
        | ToolId::AdminUsersReinstate
        | ToolId::AdminInviteCodesList
        | ToolId::AdminInviteCodesCreate
        | ToolId::AdminInviteCodesDelete
        | ToolId::AdminInviteRequestsList
        | ToolId::AdminInviteRequestsApprove
        | ToolId::AdminInviteRequestsDeny
        | ToolId::AdminRegistrationModeSet) => {
            admin_users_call(AdminClient::new(rpc), id, args).await
        }
        id @ (ToolId::AdminTierCreate
        | ToolId::AdminTierUpdate
        | ToolId::AdminTierDelete
        | ToolId::AdminMembershipTiersList
        | ToolId::AdminMembershipTierSet
        | ToolId::AdminMembershipTierClear
        | ToolId::AdminRegionGet
        | ToolId::AdminRegionSet
        | ToolId::AdminWebAppOriginGet
        | ToolId::AdminWebAppOriginSet
        | ToolId::AdminUpdateChannelGet
        | ToolId::AdminUpdateChannelSet
        | ToolId::AdminServicesList
        | ToolId::AdminServiceSet
        | ToolId::AdminServingPortSet
        | ToolId::AdminHostRestart
        | ToolId::AdminSigningKeys
        | ToolId::AdminReportsList
        | ToolId::AdminReportResolve
        | ToolId::AdminBlockedList
        | ToolId::AdminReportBlock
        | ToolId::AdminLogs
        | ToolId::AdminAuditList
        | ToolId::AdminAuditIntegrity
        | ToolId::AdminApexActorGet
        | ToolId::AdminApexActorSet
        | ToolId::AdminDnsRecords
        | ToolId::AdminDnsVerify
        | ToolId::AdminCertStatus
        | ToolId::AdminDomainsList
        | ToolId::AdminDomainRemove
        | ToolId::AdminDomainRestore
        | ToolId::AdminFeatureLimitsGet
        | ToolId::AdminFeatureLimitSet
        | ToolId::AdminContentRulesGet
        | ToolId::AdminContentRulesSet
        | ToolId::AdminTransportGet
        | ToolId::AdminTransportSet
        | ToolId::AdminSubhandlesSet
        | ToolId::AdminStorageCapSet
        | ToolId::AdminCorsSet
        | ToolId::AdminGc
        | ToolId::AdminWorkerStatus
        | ToolId::AdminClusterStatus
        | ToolId::AdminDomainAdd
        | ToolId::AdminDomainConfigSet
        | ToolId::AdminCatchAllSet
        | ToolId::AdminRoleAddressSet
        | ToolId::AdminDkimIntervalSet
        | ToolId::AdminDomainRenameStatus
        | ToolId::AdminDomainRenamesList
        | ToolId::AdminDkimSelectorsList) => admin_nest_side_call(rpc, id, args).await,
        id @ (ToolId::AdminMailSettings
        | ToolId::AdminMailHealth
        | ToolId::AdminMailEnabledSet
        | ToolId::AdminMailAutoEnableSet
        | ToolId::AdminMailPolicySet
        | ToolId::AdminMailDeliverabilityCheck
        | ToolId::AdminMailBlocklistCheck
        | ToolId::AdminMailWarmupReset
        | ToolId::AdminMailSpamBaselinePublish
        | ToolId::AdminMailReportsDmarc
        | ToolId::AdminMailReportsForensic
        | ToolId::AdminMailReportsTls
        | ToolId::AdminMailReportsDmarcGet
        | ToolId::AdminMailReportsForensicGet
        | ToolId::AdminMailReportsTlsGet
        | ToolId::AdminMailReportsBlocklistHistory
        | ToolId::AdminMailReportsDiagnosticRuns
        | ToolId::AdminForwardersList
        | ToolId::AdminForwardersCreate
        | ToolId::AdminForwardersDelete
        | ToolId::AdminCalendarPortGet
        | ToolId::AdminCalendarEnabledSet
        | ToolId::AdminCalendarPortSet
        | ToolId::AdminContactsEnabledSet
        | ToolId::AdminFilesEnabledSet
        | ToolId::AdminBridgesPendingList
        | ToolId::AdminBridgesApprovedList) => {
            admin_mail_call(MailAdminClient::new(rpc), id, args).await
        }
        ToolId::PostDelete => {
            let request = PostAuthorDeleteRequest {
                post_id: str_arg(args, "post_id")?.to_owned(),
                ..Default::default()
            };
            let reply: PostAuthorDeleteReply = rpc
                .request(KIND_DELETE, request)
                .await
                .map_err(ToolError::from_rpc)?;
            data(&reply)
        }
        // A poll is the list and the unread count in one reply, and never
        // marks read (`apps/cli.md` § The feature ledger, `notifications`
        // degraded to a poll). The muted-word collapse is not applied: the
        // list is the user's sealed moderation state, which no arm of this
        // catalog reaches — the tool's description says so.
        ToolId::NotificationsPoll => {
            let client = NotificationsClient::new(rpc);
            let page = client
                .notifications_list(opt_i64(args, "cursor"), opt_i64(args, "limit"))
                .await
                .map_err(ToolError::from_rpc)?;
            let unread = client
                .notifications_count()
                .await
                .map_err(ToolError::from_rpc)?;
            Ok(json!({
                "notifications": data(&page.notifications)?,
                "cursor": page.cursor,
                "unread": unread.count,
            }))
        }
        ToolId::NotificationsMarkRead => data(
            &NotificationsClient::new(rpc)
                .notifications_mark_read(opt_i64(args, "up_to"))
                .await
                .map_err(ToolError::from_rpc)?,
        ),
        // The custodian answers the profile it stored; the tool answers its
        // display fields through the same decode `nest://profile` reads.
        ToolId::ProfileSet => {
            let links = args
                .get("links")
                .map(|v| {
                    serde_json::from_value(v.clone())
                        .map_err(|e| ToolError::new(format!("links: {e}")))
                })
                .transpose()?;
            let request = ProfileAuthorSetRequest {
                display_name: opt_str(args, "display_name"),
                bio: opt_str(args, "bio"),
                links,
                ..Default::default()
            };
            let reply: ProfileAuthorSetReply = rpc
                .request(KIND_SET, request)
                .await
                .map_err(ToolError::from_rpc)?;
            let display = decode_profile_display(&reply.body)
                .map_err(|e| ToolError::new(format!("the stored profile did not decode: {e}")))?;
            Ok(profile_json(&display))
        }
    }
}

/// Read the resource `uri` names; its contents as `resources/read` carries
/// them. `None` for a URI the catalog serves no resource at. A read, so a
/// session that drops under it is redialled and the read run again.
pub async fn read_resource<H: Host>(
    host: &H,
    uri: &str,
) -> Option<Result<(&'static Resource, Value), ToolError>> {
    let (resource, actor, account) = catalog::resource(uri)?;
    Some(
        async {
            let rpc = host.session(account).await?;
            let actor = match actor {
                Some(actor) => actor.to_owned(),
                None => host.account(account).await?,
            };
            let display = match profile_read(rpc.clone(), &actor).await {
                Err(e) if e.connection_lost => {
                    profile_read(host.redial(account, &rpc).await?, &actor).await
                }
                read => read,
            }?;
            Ok((resource, profile_json(&display)))
        }
        .await,
    )
}

async fn profile_read<R>(rpc: R, actor: &str) -> Result<ProfileDisplay, ToolError>
where
    R: RpcRequester<Error: RpcErrorClass>,
{
    let reply = ProfileClient::new(rpc)
        .profile_get(actor.to_owned())
        .await
        .map_err(ToolError::from_rpc)?;
    decode_profile_display(&reply.body)
        .map_err(|e| ToolError::new(format!("the stored profile did not decode: {e}")))
}

/// The decoded display fields, as the resource's JSON — the one shared decode
/// every app's profile page renders from.
fn profile_json(p: &ProfileDisplay) -> Value {
    json!({
        "display_name": p.display_name,
        "bio": p.bio,
        "links": p.links.iter().map(|l| json!({ "label": l.label, "uri": l.uri })).collect::<Vec<_>>(),
        "avatar": p.avatar.as_ref().map(ToString::to_string),
        "banner": p.banner.as_ref().map(ToString::to_string),
    })
}
