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
use fauna_client_mail_settings::admin_policy::parse_knob_u64;
use fauna_client_mail_settings::caldav_policy::CaldavPolicyMachine;
use fauna_client_mail_settings::domain_forms::{add_domain_possibility, remove_possibility};
use fauna_client_mail_settings::forwarders::ForwarderMachine;
use fauna_client_mail_settings::local_domains::LocalDomainsSnapshot;
use fauna_client_moderation::ModerationClient;
use fauna_client_notifications::NotificationsClient;
use fauna_client_profile::{ProfileClient, ProfileDisplay, decode_profile_display};
use fauna_client_web::WebClient;
use fauna_core::content_policy::{ContentPolicyDocument, decode_document};
use fauna_core::data::PostBody;
use fauna_core::feature_gate::FeaturePolicy;
use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;
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

use crate::catalog::{self, Resource, ResourceId, Tool, ToolId};
use crate::reach::Reach;

/// Why a shell has no principal session to call through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionUnavailable {
    /// No consent has started: the human has to be asked for their handle.
    NotConnected,
    /// A consent is waiting on the human to type `user_code`.
    Pending { user_code: String },
    /// A consent asking for the admin arms is waiting on the human to type
    /// `user_code` — whose approval by an account holding no admin authority
    /// resolves nothing (`authorization-server.md` § Scope grammar → *The
    /// admin arms*), so its answer is [`SessionUnavailable::Pending`]'s
    /// followed by [`ADMIN_APPROVAL_WAITS`], which names the way out.
    PendingAdmin { user_code: String },
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

/// What a `connect` that asked for the admin arms adds to its waiting answer:
/// an approval by an account holding no admin authority resolves nothing
/// (`authorization-server.md` § Scope grammar → *The admin arms*), so the
/// sentence names the way out — connecting without the admin arms
/// (`apps/cli.md` § A tool that cannot run, rule 3). "A few seconds" covers
/// the poll interval between an admin's approval and the connection.
// public-text: cli-answer-strings-public
pub const ADMIN_APPROVAL_WAITS: &str = "This connection asks for the admin permissions, which only an account that administers its server can approve: if it is still waiting a few seconds after the user approved it, their account does not administer its server — call connect again without admin: true.";
// public-text-end: cli-answer-strings-public

/// What a `connect` that asked for the admin arms answers once its code
/// expired unapproved: the same way out as [`ADMIN_APPROVAL_WAITS`], in the
/// past tense.
// public-text: cli-answer-strings-public
pub const ADMIN_CONSENT_EXPIRED: &str = "The code expired before the connection was approved. This connection asked for the admin permissions, which only an account that administers its server can approve: if the user approved it, their account does not administer its server — call connect again without admin: true.";
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

    /// What the held connections' granted scopes reach — what `tools/list`
    /// lists on a shell whose transport announces changes
    /// ([`crate::reach`]). A shell that stores no granted scope lists the
    /// whole catalog.
    async fn reach(&self) -> Reach {
        Reach::Whole
    }

    /// Resolves once [`Host::reach`] may have changed — a connection landed,
    /// failed, was revoked or added — so the stdio transport can announce
    /// `notifications/tools/list_changed`. One consumer; a change before it
    /// waits is not lost. A shell whose reach never changes never resolves.
    async fn reach_changed(&self) {
        std::future::pending::<()>().await
    }

    /// Whether this shell has the conversations oracle's door: one without
    /// never lists the conversations tools (rule 1) and answers a call to
    /// one with [`NO_CONVERSATIONS_DOOR`].
    fn conversations_door(&self) -> bool {
        false
    }

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
    /// A precondition the nest refused, with its reason — worded with the
    /// step that unblocks it once the tool is known ([`ToolError::worded`]).
    refusal: Option<Refusal>,
    /// A fault, not a refusal: the transport, or a nest error the model can
    /// do nothing about — the one class a resource answers with JSON-RPC's
    /// internal-error code (`apps/cli.md` § A tool that cannot run, rule 3).
    fault: bool,
}

/// A nest refusal the model can act on, sorted by its wire code's family and
/// carrying the reason the nest put in its details — the reason survives into
/// the sentence (`apps/cli.md` § A tool that cannot run, rule 3).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refusal {
    /// The grant does not reach the kind: a scope this connection lacks.
    Scope(String),
    /// An admin-plane kind, and the account holds no admin authority.
    NoAuthority(String),
    /// The id or name passed names nothing the nest holds.
    NotFound(String),
    /// Any other refusal of the call as made.
    Refused(String),
}

/// The principal gate's two reasons (`principal_handlers::dispatch_resolved`),
/// matched in the refusal's details.
const SCOPE_REASON: &str = "not covered by the principal's scopes";
const NO_AUTHORITY_REASON: &str = "holds no admin authority";

/// Code families that are faults, never a precondition the model can meet.
const FAULT_FAMILIES: &[&str] = &[
    "internal",
    "encode_failed",
    "timeout",
    "cancelled",
    "disconnected",
    "rate_limited",
    "unknown_kind",
];

impl Refusal {
    /// The refusal `r` is, or `None` for a fault — and for a code the shared
    /// rendering already words exactly (`RpcError::localized`'s exact arms),
    /// whose sentence stands.
    fn of(r: &fauna_protocol::RpcError) -> Option<Self> {
        use fauna_i18n::strings::error;
        let family = r.code_suffix();
        if FAULT_FAMILIES.contains(&family) {
            return None;
        }
        let generic = [
            error::UNEXPECTED,
            error::AUTHORIZATION,
            error::protocol::MALFORMED,
        ];
        if !generic.contains(&r.localized()) {
            return None;
        }
        let detail = r.detail_or_code();
        let reason = if detail.trim().is_empty() || detail == r.code {
            family.replace('_', " ")
        } else {
            detail.trim().to_owned()
        };
        Some(if reason.contains(SCOPE_REASON) {
            Self::Scope(reason)
        } else if reason.contains(NO_AUTHORITY_REASON) {
            Self::NoAuthority(reason)
        } else if matches!(family, "not_found" | "tier_not_found") {
            Self::NotFound(reason)
        } else {
            Self::Refused(reason)
        })
    }
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
            connection_lost: false,
            refusal: None,
            fault: false,
        }
    }

    fn from_rpc<E: RpcErrorClass + core::fmt::Display>(e: E) -> Self {
        let rpc = e.as_rpc_error();
        let refusal = rpc.and_then(Refusal::of);
        Self {
            code: rpc.map(|r| r.code.clone()),
            message: e.to_string(),
            connection_lost: e.is_connection_lost(),
            fault: rpc.is_none_or(|r| FAULT_FAMILIES.contains(&r.code_suffix())),
            refusal,
        }
    }

    /// Whether this is a fault rather than a refusal — what a resource read
    /// answers with the internal-error code, and nothing else does.
    pub fn is_fault(&self) -> bool {
        self.fault
    }

    /// Whether the nest refused because the id or name passed names nothing.
    pub fn is_not_found(&self) -> bool {
        matches!(self.refusal, Some(Refusal::NotFound(_)))
    }

    /// Word a nest refusal for the tool (or resource) that met it: the
    /// nest's reason, then the step that unblocks it — a catalog tool, the
    /// person's own app, or, when nothing on this account can, a plain stop
    /// (`apps/cli.md` § A tool that cannot run, rule 3). A fault keeps its
    /// sentence.
    fn worded(mut self, name: &str, arms: &[&str], step: Option<String>) -> Self {
        let Some(refusal) = &self.refusal else {
            return self;
        };
        self.message = match refusal {
            Refusal::Scope(reason) if arms.iter().any(|a| catalog::is_admin_arm(a)) => format!(
                "This account's connection does not carry the admin permission {name} needs ({reason}). If the user administers their Fauna server, call connect with their handle and admin: true; if they do not, no admin tool can act for this account."
            ),
            Refusal::Scope(reason) => format!(
                "This account's connection does not carry the permission {name} needs ({reason}). Call connect with the user's handle again so they can approve it."
            ),
            Refusal::NoAuthority(reason) => format!(
                "This account does not administer its Fauna server ({reason}), so {name} cannot act for it and connecting again changes nothing: only an admin of that server can do this. Do not offer admin tools for this account."
            ),
            Refusal::NotFound(reason) => format!(
                "The nest found nothing to act on ({reason}). {}",
                step.unwrap_or_else(|| {
                    "Check the id or name passed against what a list tool answers.".into()
                })
            ),
            Refusal::Refused(reason) => format!(
                "The nest refused this call as made ({reason}). Change what the reason names before calling it again; the same call will be refused the same way."
            ),
        };
        self
    }

    /// [`ToolError::worded`] for a catalog tool.
    fn for_tool(self, tool: &Tool) -> Self {
        self.worded(tool.name, tool.arms, refusal_step(tool.id))
    }

    /// The text of the MCP error content block.
    pub fn text(&self) -> String {
        match &self.code {
            Some(code) => format!("{code}: {}", self.message),
            None => self.message.clone(),
        }
    }

    /// A refusal worded from a shared reason — the same string-table sentence
    /// the seven apps paint.
    fn localized(reason: &LocalizedText) -> Self {
        Self::new(reason.resolve(fauna_i18n::strings::lookup))
    }
}

/// The step that makes a not-found precondition true for `id`: the list
/// tool that answers a valid id or name, and where none can, the place that
/// can (`apps/cli.md` § A tool that cannot run, rule 3).
fn refusal_step(id: ToolId) -> Option<String> {
    use ToolId::*;
    Some(match id {
        FeedGet | FeedPosts => "Call feed_list for this account's feeds and pass an id it lists.",
        PostDelete => {
            "Call feed_local for the posts on this account's server and pass the id of one this account published with post_create."
        }
        AdminUsersGet | AdminUsersUpdate | AdminUsersSuspend | AdminUsersReinstate => {
            "Call admin_users_list for this server's accounts and pass an id it lists."
        }
        AdminInviteCodesDelete => "Call admin_invite_codes_list and pass a code it lists.",
        AdminInviteRequestsApprove | AdminInviteRequestsDeny => {
            "Call admin_invite_requests_list and pass a request it lists."
        }
        AdminTierUpdate | AdminTierDelete => {
            "Call admin_tiers_list for this server's quota tiers and pass a name it lists."
        }
        AdminMembershipTierSet | AdminMembershipTierClear => {
            "Call admin_membership_tiers_list for the subscription tiers and admin_tiers_list for the quota tiers, and pass names they list; a quota tier that does not exist yet is made with admin_tier_create first."
        }
        AdminReportResolve | AdminReportBlock => {
            "Call admin_reports_list and pass a report it lists."
        }
        AdminDomainRemove | AdminDomainRestore | AdminDomainConfigSet | AdminCatchAllSet
        | AdminRoleAddressSet | AdminDkimIntervalSet | AdminDnsRecords | AdminDnsVerify => {
            "Call admin_domains_list for this server's domains and pass one it lists. If it lists none, admin_domain_add says where the first one is added."
        }
        AdminDomainRenameStatus => {
            "Call admin_domain_renames_list and pass a rename it lists."
        }
        AdminMailReportsDmarcGet => "Call admin_mail_reports_dmarc and pass a report key it lists.",
        AdminMailReportsForensicGet => {
            "Call admin_mail_reports_forensic and pass a report key it lists."
        }
        AdminMailReportsTlsGet => "Call admin_mail_reports_tls and pass a report key it lists.",
        AdminForwardersDelete => "Call admin_forwarders_list and pass a forwarder it lists.",
        _ => return None,
    }
    .to_owned())
}

/// Ask a shared predicate before the call is sent (`apps/cli.md` § A tool
/// that cannot run, rule 5): a blocked draft refuses with the predicate's own
/// reason and nothing reaches the nest.
fn possible(answer: Possibility) -> Result<(), ToolError> {
    match answer {
        Possibility::Possible => Ok(()),
        Possibility::Blocked { reason } => Err(ToolError::localized(&reason)),
        // No draft predicate answers absent; were one to, the tool still
        // refuses rather than sending a call the apps would not offer.
        Possibility::Absent => Err(ToolError::new("this cannot be done on this server")),
    }
}

/// A draft argument as the apps' form holds it: the string typed, or a JSON
/// number written out — so the predicate judges what the model passed.
fn draft_arg(args: &Value, name: &str) -> String {
    match args.get(name) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
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
            SessionUnavailable::PendingAdmin { user_code } => ToolError::new(format!(
                "Waiting for the user to approve. They type {user_code} into Connect an app in one of their own Fauna apps, then approve the request. {ADMIN_APPROVAL_WAITS}"
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
            let actor = draft_arg(args, "actor_id");
            possible(fauna_client_admin::admit_actor_possibility(&actor))?;
            let (guardian, band) = supervision_args(args)?;
            admin
                .users_create(
                    fauna_client_admin::parse_admit_actor(&actor).expect("the predicate passed it"),
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
                Some(_) => {
                    let draft = draft_arg(args, "max_free_users");
                    possible(fauna_client_admin::registration_ceiling_possibility(&draft))?;
                    // A blank draft is the form's "no limit"; the tool's is null.
                    Some(
                        fauna_client_admin::parse_registration_ceiling(&draft)
                            .map_err(|r| ToolError::localized(&r))?
                            .ok_or_else(|| {
                                ToolError::localized(&LocalizedText::key(
                                    "admin.users_page.max_free_users_invalid",
                                ))
                            })?,
                    )
                }
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
            let caps = [
                "max_inbox_bytes",
                "max_storage_bytes",
                "max_devices",
                "max_blob_size",
                "max_feeds",
            ]
            .map(|cap| draft_arg(args, cap));
            possible(fauna_client_admin::tier_create_possibility(
                &draft_arg(args, "name"),
                caps.each_ref().map(String::as_str),
            ))?;
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
            possible(fauna_client_admin::membership_save_possibility(&draft_arg(
                args,
                "admin_tier",
            )))?;
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
            let region = nullable_str(args, "region")?;
            if let Some(code) = region {
                possible(fauna_client_admin::region_possibility(code))?;
            }
            let region = region
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
            // Ask the shared answer first (`web-content-hosting.md` § While the
            // central origin is not live, the choice is not offered, rule 5):
            // a central mode the nest says it cannot serve is refused here with
            // the nest's own reason, and nothing is sent.
            let admin = admin!();
            if mode == WebAppOrigin::Central {
                let status = admin.web_app_origin().await.map_err(ToolError::from_rpc)?;
                let view = fauna_client_admin::admin_web_app_origin_view(status.as_ref());
                possible(fauna_client_admin::web_app_origin_set_possibility(
                    Some(&view),
                    Some(mode),
                ))?;
            }
            data(
                &admin
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
            let draft = draft_arg(args, "port");
            possible(fauna_client_admin::serving_port_possibility(&draft))?;
            let port = fauna_core::format::parse_port(&draft).expect("the predicate passed it");
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
            let domain = str_arg(args, "domain")?;
            let listed: ListLocalDomainsReply = rpc
                .request(
                    "fauna.bridges.list_local_domains",
                    ListLocalDomainsRequest {},
                )
                .await
                .map_err(ToolError::from_rpc)?;
            let domains = LocalDomainsSnapshot::from_rows(listed.active, Vec::new());
            // A domain the list does not hold is the nest's own refusal.
            if let Some(row) = domains
                .active
                .iter()
                .find(|d| d.domain.eq_ignore_ascii_case(domain.trim()))
            {
                possible(remove_possibility(row))?;
            }
            let reply: RemoveLocalDomainReply = rpc
                .request(
                    "fauna.bridges.remove_local_domain",
                    RemoveLocalDomainRequest {
                        domain: domain.to_owned(),
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
                    SaveError::Invalid(reason) => ToolError::localized(&reason),
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
                    "this server has no active domain yet, and the first domain added becomes its primary domain, which can never be removed, only renamed onto a different domain: the admin adds that one in their own Fauna app",
                ));
                // public-text-end: cli-answer-strings-public
            }
            // Blank, malformed or already active: the form's own refusal — an
            // add of an active domain would answer success for nothing.
            let draft = draft_arg(args, "domain");
            let domains = LocalDomainsSnapshot::from_rows(listed.active, Vec::new());
            possible(add_domain_possibility(Some(&domains), &draft))?;
            let reply: AddLocalDomainReply = rpc
                .request(
                    "fauna.bridges.add_local_domain",
                    AddLocalDomainRequest {
                        domain: draft.trim().to_owned(),
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
        // A number typed as text is the page's draft: parsed by the shared
        // knob parser and refused with its sentence, never sent as a string.
        let value = match (&fields[name], value) {
            (Value::Number(n), Value::String(draft)) if n.is_u64() => {
                Value::from(parse_knob_u64(draft).map_err(|r| ToolError::localized(&r))?)
            }
            _ => value.clone(),
        };
        fields.insert(name.clone(), value);
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
            if changes.as_object().is_some_and(serde_json::Map::is_empty) {
                return Err(ToolError::new(
                    "values names no field to change, so nothing would be saved: pass the fields to change (admin_mail_settings names them)",
                ));
            }
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
            possible(ForwarderMachine::create_draft_possibility(
                &draft_arg(args, "local_domain"),
                &draft_arg(args, "pattern"),
                &draft_arg(args, "forward_target"),
            ))?;
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
            let draft = draft_arg(args, "port");
            possible(CaldavPolicyMachine::port_draft_possibility(&draft))?;
            let port = fauna_core::format::parse_port(&draft).expect("the predicate passed it");
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
    // Non-empty on top of the shared decoder: an empty id is never valid here.
    if s.is_empty() {
        return None;
    }
    fauna_core::format::hex_decode(s)
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
    call_unworded(host, tool, args)
        .await
        .map_err(|e| e.for_tool(tool))
}

/// [`call`] before a nest refusal is worded for the tool.
async fn call_unworded<H: Host>(host: &H, tool: &Tool, args: &Value) -> Result<Value, ToolError> {
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
            if ["display_name", "bio", "links"]
                .iter()
                .all(|f| args.get(f).is_none_or(Value::is_null))
            {
                return Err(ToolError::new(
                    "no field to change was passed, so nothing would be saved: pass display_name, bio or links",
                ));
            }
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
            let display = decode_profile_display(&reply.body).map_err(|e| ToolError {
                fault: true,
                ..ToolError::new(format!("the stored profile did not decode: {e}"))
            })?;
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
        .await
        .map_err(|e: ToolError| {
            let step = (resource.id == ResourceId::Profile).then(|| {
                "Pass the hex actor id of an account this nest knows, as a notification's sender_id or a post's author gives it.".to_owned()
            });
            e.worded(resource.name, resource.arms, step)
        }),
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
    decode_profile_display(&reply.body).map_err(|e| ToolError {
        fault: true,
        ..ToolError::new(format!("the stored profile did not decode: {e}"))
    })
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
