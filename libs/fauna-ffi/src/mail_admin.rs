//! Re-exports the admin + user-tier mail machines so their UniFFI exports surface
//! in the generated Swift / Kotlin / C# bindings, plus free-fn constructors that
//! build each machine over an [`FfiNestClient`]'s WS-RPC connection.
//!
//! The machines themselves live in their home crates — `fauna-client-dns`
//! ([`DnsManagementMachine`], the `admin-dns` page) and
//! `fauna-client-mail-settings` (the admin `LocalDomainMachine` /
//! `BridgeApprovalMachine` / `ForwarderMachine` pages,
//! plus the user-tier mail-settings family — `MailSettingsMachine`,
//! `MailAliasesMachine`, and the `mail-lists` / `mail-list-members` / `mail-export`
//! / `mail-spam` pages: [`MailListsMachine`] / [`MailListMembersMachine`] /
//! [`MailExportMachine`] / [`MailSpamMachine`]). This file is a thin glue layer mirroring
//! `src/onboarding.rs`: it re-exports the machine + snapshot + action + error
//! types and wraps each crate's native `build_*_machine` seam constructor in an
//! `#[uniffi::export]` free fn that takes the shared connection handle.
//!
//! Per priority #2, the snapshot projection + action sequencing + the WS-RPC
//! seam glue all live in shared Rust; the per-app (windows/macos/ios/android)
//! UI only constructs a machine here and renders its snapshot / dispatches its
//! actions. Linux builds the same machines natively via the same crates.
//!
//! ## Error-type naming
//!
//! `fauna-client-dns` and `fauna-client-mail-settings` each define a
//! `DispatchError` + `NestError`, but UniFFI requires unique type idents across
//! the whole library and keys on the Rust ident (not the re-export alias). The
//! dns crate's are therefore named `DnsDispatchError` / `DnsNestError`; the
//! three mail-settings machines all surface the one shared
//! `fauna_client_mail_settings::DispatchError` (exported once, no collision).

use std::sync::Arc;

pub use fauna_client_dns::{
    CertHealthState, CertStatusRow, CredentialSummary, DelegationView, DnsAction,
    DnsCredentialField, DnsDispatchError, DnsManagementMachine, DnsNestError, DnsRecordRow,
    DnsSnapshot, DnsStatus, DomainView, PendingCertIssue, RecordVerdict, VerifyStatus,
};
pub use fauna_client_mail_settings::{
    AliasKind, AliasPolicyView, AliasView, AliasesStatus, ApprovedBridgeView, AuthPolicyView,
    BridgeApprovalAction, BridgeApprovalMachine, BridgeApprovalSnapshot, BridgeApprovalStatus,
    CaldavPolicyAction, CaldavPolicyMachine, CaldavPolicySnapshot, CaldavPolicyStatus,
    CarddavPolicyAction, CarddavPolicyMachine, CarddavPolicySnapshot, CarddavPolicyStatus,
    CredentialKind, DispatchError, ExportFormat, ExportScope, ExportSessionState,
    ExportSessionView, ExportStatus, ExportStep, ForwarderAction, ForwarderMachine,
    ForwarderStatus, ForwarderView, ForwardersSnapshot, ImapPolicyView, ImportResult,
    ImportSessionState, ImportSourceKind, ImportStatus, ImportStep, ImportTlsMode, ListDraft,
    ListMembers, ListView, ListsStatus, LocalDomainAction, LocalDomainMachine, LocalDomainStatus,
    LocalDomainView, LocalDomainsSnapshot, MailAliasesAction, MailAliasesMachine,
    MailAliasesSnapshot, MailCredentialSummary, MailExportAction, MailExportMachine,
    MailExportSnapshot, MailImportAction, MailImportMachine, MailImportSnapshot,
    MailListMembersAction, MailListMembersMachine, MailListMembersSnapshot, MailListsAction,
    MailListsMachine, MailListsSnapshot, MailPolicyAction, MailPolicyMachine, MailPolicySnapshot,
    MailPolicyStatus, MailSettingsAction, MailSettingsMachine, MailSettingsSnapshot,
    MailSpamAction, MailSpamMachine, MailSpamSnapshot, MailboxOption, MailboxProgressView,
    MemberStatus, MemberView, MuaInstructions, NestError, OutboundPolicyView, PendingBridgeView,
    PendingRotationStatus, PrimaryDomainRenameView, SettingsStatus, SourceMailboxOption,
    SpamHistory, SpamPolicyView, SpamStatus, SpamTrainingView, SubmissionPolicyView, TrainingLabel,
    TrainingSource, WebdavPolicyAction, WebdavPolicyMachine, WebdavPolicySnapshot,
    WebdavPolicyStatus,
};

use crate::{FfiError, FfiNestClient, general_err};

/// Build a [`DnsManagementMachine`] for the `admin-dns` page over `nest`'s
/// WS-RPC connection. Read/verify only — no credential store, so the managed-mode
/// + credential actions reject with `InvalidState`. Use
/// [`build_dns_management_machine_with_credentials`] for the mode toggle.
#[uniffi::export]
pub fn build_dns_management_machine(nest: Arc<FfiNestClient>) -> Arc<DnsManagementMachine> {
    Arc::new(fauna_client_dns::build_dns_management_machine(
        nest.nest_arc(),
    ))
}

/// Build a [`DnsManagementMachine`] with the client-held credential store wired —
/// the variant the `admin-dns` per-domain mode toggle (`admin-dns-domain-mode`) +
/// credential actions need. Reads and writes the account's DNS record
/// (`fauna.state.dns`) through this seat's account runtime, waiting for the
/// runtime to come up when it is not yet (so onboarding's seal-at-capture may
/// call this right at sign-in); takes the actor's 32-byte ed25519 `secret` for
/// the issuance seal, and wires the DNS-provider seam (with the e2e fake-provider decorator under
/// `FAUNA_DNS_PROVIDER_FAKE`). Opting a domain in to managed without a held
/// credential whose zones cover it still rejects with `InvalidState`. Mirrors
/// linux `FaunaClient::dns_set_mode`'s
/// `build_dns_management_machine_with_credentials`. Returns `Result` because the
/// keypair derivation is fallible.
#[uniffi::export]
pub fn build_dns_management_machine_with_credentials(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
) -> Result<Arc<DnsManagementMachine>, FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    Ok(Arc::new(
        fauna_client_dns::build_dns_management_machine_with_credentials(
            nest.nest_arc(),
            keypair,
            Arc::new(crate::account_runtime::handle),
        ),
    ))
}

/// Build a [`LocalDomainMachine`] for the `admin-dns` page over `nest`'s
/// WS-RPC connection.
#[uniffi::export]
pub fn build_local_domains_machine(nest: Arc<FfiNestClient>) -> Arc<LocalDomainMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_local_domains_machine(nest.nest_arc()))
}

/// Build a [`BridgeApprovalMachine`] for the `admin-bridges-pending` page over
/// `nest`'s WS-RPC connection.
#[uniffi::export]
pub fn build_bridge_approval_machine(nest: Arc<FfiNestClient>) -> Arc<BridgeApprovalMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_bridge_approval_machine(nest.nest_arc()))
}

/// Build a [`MailPolicyMachine`] for the flat `admin-mail` policy form
/// (`admin.md` § Mail / `mail-policy-config.md` Tier 2) over `nest`'s WS-RPC
/// connection. An admin machine like [`build_forwarders_machine`]: it needs only
/// the connection handle (the `get_mail_config` read twin + `set_mail_enabled` +
/// `put_<substruct>_policy` writes are all Admin-class, no actor secret).
#[uniffi::export]
pub fn build_mail_policy_machine(nest: Arc<FfiNestClient>) -> Arc<MailPolicyMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_mail_policy_machine(nest.nest_arc()))
}

/// Build a [`CaldavPolicyMachine`] for the flat `admin-calendar` page (`admin.md`
/// § 8 Calendar / `caldav-server.md` § Independent enablement) over `nest`'s
/// WS-RPC connection — the CalDAV-enable sibling of [`build_mail_policy_machine`].
/// An admin machine that needs only the connection handle: it hydrates
/// `caldav_enabled` from the `get_mail_config` read twin and saves via
/// `set_caldav_enabled` (both Admin-class, no actor secret). The native apps
/// (windows / macos / ios / android) lift the `admin-calendar` page over this.
#[uniffi::export]
pub fn build_caldav_policy_machine(nest: Arc<FfiNestClient>) -> Arc<CaldavPolicyMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_caldav_policy_machine(nest.nest_arc()))
}

/// Build a [`CarddavPolicyMachine`] for the flat `admin-contacts` page
/// (`admin.md` § Contacts / `carddav-server.md` § Independent enablement) over
/// `nest`'s WS-RPC connection — the CardDAV-enable contacts sibling of
/// [`build_caldav_policy_machine`]. An admin machine that needs only the
/// connection handle: it hydrates `carddav_enabled` from the `get_mail_config`
/// read twin and saves via `set_carddav_enabled` (both Admin-class, no actor
/// secret). The native apps (windows / macos / ios / android) lift the
/// `admin-contacts` page over this.
#[uniffi::export]
pub fn build_carddav_policy_machine(nest: Arc<FfiNestClient>) -> Arc<CarddavPolicyMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_carddav_policy_machine(nest.nest_arc()))
}

/// Build a [`WebdavPolicyMachine`] for the flat `admin-files` page
/// (`admin.md` § Files / `webdav-server.md` § Independent enablement) over
/// `nest`'s WS-RPC connection — the WebDAV-enable files sibling of
/// [`build_carddav_policy_machine`]. An admin machine that needs only the
/// connection handle: it hydrates `webdav_enabled` from the `get_mail_config`
/// read twin and saves via `set_webdav_enabled` (both Admin-class, no actor
/// secret). The native apps (windows / macos / ios / android) lift the
/// `admin-files` page over this.
#[uniffi::export]
pub fn build_webdav_policy_machine(nest: Arc<FfiNestClient>) -> Arc<WebdavPolicyMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_webdav_policy_machine(nest.nest_arc()))
}

/// Build a [`ForwarderMachine`] for the `admin-aliases` page — admin external
/// forwarders (admin.md § 4 / mail-aliases.md § Kind 7) — over `nest`'s WS-RPC
/// connection. An admin machine like [`build_local_domains_machine`]: it needs
/// only the connection handle (the `fauna.bridges.{list,create,delete}_forwarder`
/// + `list_local_domains` kinds are Admin-class, no actor secret).
#[uniffi::export]
pub fn build_forwarders_machine(nest: Arc<FfiNestClient>) -> Arc<ForwarderMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_forwarders_machine(nest.nest_arc()))
}

/// Build a [`MailSettingsMachine`] for the user-facing `mail-settings` page over
/// `nest`'s WS-RPC connection. Unlike the admin machines, this one needs the
/// actor's 32-byte ed25519 `secret` (to sign submission tokens + derive the
/// account's mail custody) and the deployment `node_url` (to derive the MUA connection
/// details), so the constructor takes those alongside the connection handle.
/// Returns `Result` because the keypair derivation is fallible.
#[uniffi::export]
pub fn build_mail_settings_machine(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    node_url: String,
) -> Result<Arc<MailSettingsMachine>, FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    Ok(Arc::new(
        fauna_client_mail_settings::rpc_glue::build_mail_settings_machine(
            nest.nest_arc(),
            keypair,
            crate::account_runtime::mail_store(),
            &node_url,
            crate::ledger_seam(),
            Some(Arc::new(fauna_client_folders::CustodyServedSets(
                crate::account_runtime::folder_key_store(),
            ))),
        ),
    ))
}

/// The post-claim serving enablement (`onboarding.md` § 3b *Mechanism*) — the
/// ONE call an app's `LoggedIn` handoff makes with the four intents it read off
/// the onboarding machine (`email_enable_requested` and siblings), replacing
/// the per-app firing of the mail provision, the three DAV toggles and their
/// companion mints. Runs the shared
/// `fauna_client_mail_settings::serving_enablement` plan to the end over
/// `nest`'s connection and resolves once every step has answered (each logs its
/// own failure, so this only errs on an undecodable `secret`). Progress is
/// [`serving_enablement_json`].
#[fauna_uniffi_async::export]
pub async fn apply_post_claim_serving_enablement(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    node_url: String,
    email: bool,
    caldav: bool,
    carddav: bool,
    webdav: bool,
) -> Result<(), FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    fauna_client_mail_settings::rpc_glue::dispatch_post_claim_serving_enablement(
        nest.nest_arc(),
        keypair,
        crate::account_runtime::mail_store(),
        node_url,
        crate::ledger_seam(),
        fauna_client_mail_settings::serving_enablement::ServingEnablementIntents {
            email,
            caldav,
            carddav,
            webdav,
        },
    )
    .await;
    Ok(())
}

/// `fauna_e2e_agent::SERVING_ENABLEMENT_KEY`'s value as JSON text — the shared
/// derivation (`serving_enablement::serving_enablement_json`) for the apps whose
/// state builder is not Rust; they re-parse it rather than re-derive the shape.
#[uniffi::export]
pub fn serving_enablement_json() -> String {
    fauna_client_mail_settings::serving_enablement::serving_enablement_json_text()
}

/// Build a [`MailAliasesMachine`] for the user-facing `mail-aliases` page over
/// `nest`'s WS-RPC connection. The user-tier sibling of the admin
/// [`build_local_domains_machine`]: it drives the **User-class**
/// `MailAccountClient` (a user touches only their own aliases — the nest derives
/// the owning actor from the authenticated caller), so unlike
/// [`build_mail_settings_machine`] it needs no actor secret / `node_url`. The
/// per-app UI renders [`MailAliasesSnapshot`] + dispatches
/// [`MailAliasesAction`]; the projection / validators / `default_domain`
/// derivation / action sequencing all live in the shared
/// `fauna_client_mail_settings::aliases` machine (priority #2). See
/// `docs/goal/behavior/mail-aliases.md` § Aliases UX.
#[uniffi::export]
pub fn build_mail_aliases_machine(nest: Arc<FfiNestClient>) -> Arc<MailAliasesMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_mail_aliases_machine(nest.nest_arc()))
}

/// Build a [`MailListsMachine`] for the user-facing `mail-lists` page over
/// `nest`'s WS-RPC connection — a person's own mailing lists (a list is a sixth
/// alias kind; `mail-mass-mailing.md` § `mail-lists` page UX). User-class like
/// [`build_mail_aliases_machine`] (the nest derives the owning actor), so it
/// needs no actor secret. The list backend is unbuilt today, so the shared seam
/// returns an honest `unimplemented` rejection the page surfaces via
/// `error-message` (priority #2: the projection / validators / sequencing live in
/// the shared `fauna_client_mail_settings::lists` machine).
#[uniffi::export]
pub fn build_mail_lists_machine(nest: Arc<FfiNestClient>) -> Arc<MailListsMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_mail_lists_machine(nest.nest_arc()))
}

/// Build a [`MailListMembersMachine`] scoped to one list (`list_id_hex` from a
/// rendered [`ListView`]; `list_name` is its friendly name, the page heading) for
/// the `mail-list-members` page. In the embedded settings seed (no inter-page
/// routing yet) the per-app UI constructs it with a placeholder list id for
/// ID-conformance, exactly like the linux lead; the full app opens it scoped to
/// the list whose Members button was clicked. Returns `Result` because a
/// malformed `list_id_hex` is rejected before any RPC.
#[uniffi::export]
pub fn build_mail_list_members_machine(
    nest: Arc<FfiNestClient>,
    list_id_hex: String,
    list_name: String,
) -> Result<Arc<MailListMembersMachine>, FfiError> {
    let machine = fauna_client_mail_settings::rpc_glue::build_mail_list_members_machine(
        nest.nest_arc(),
        list_id_hex,
        list_name,
    )
    .map_err(general_err)?;
    Ok(Arc::new(machine))
}

/// Build a [`MailExportMachine`] for the user-facing `mail-export` wizard over
/// `nest`'s WS-RPC connection (`mail-export.md` § UX shape). The wizard's
/// client-side steps (Format → Scope → Confirm) drive with no nest round-trip.
///
/// **Seam-only — for a session that holds no actor secret.** Without key
/// custody `Start` answers the honest "not yet available in this app"
/// rejection rather than opening a session nothing would drive, while listing,
/// Cancel and Discard work over the real seam. Every UniFFI app (macOS, iOS,
/// windows, android) spawns `run_export` and builds
/// [`build_mail_export_machine_with_key_custody`]; each falls back to this one
/// only when it has no secret to hand over.
#[uniffi::export]
pub fn build_mail_export_machine(nest: Arc<FfiNestClient>) -> Arc<MailExportMachine> {
    Arc::new(
        fauna_client_mail_settings::rpc_glue::build_mail_export_machine_without_key_custody(
            nest.nest_arc(),
        ),
    )
}

/// Build a [`MailExportMachine`] **with key custody** — for an app whose glue
/// spawns [`MailExportMachine::run_export`] after a `Start`/`Resume` that lands
/// `Running` (custody without the spawn is the fake-green `Start` the seam-only
/// [`build_mail_export_machine`] exists to prevent). The actor's 32-byte
/// `secret` opens every record under the account's standing key set and wraps
/// the per-session blob key (`mail-export.md` § Key material); `handle` names
/// the archive's root directory and saved file (refresh it with
/// `set_actor_handle`); `save_dir` is the directory § Download flow step 5
/// writes the recovered `.zip.zst` into — a `.part` file renamed into place only
/// on a complete, terminated archive, so a refused download leaves nothing
/// there. A platform whose user-facing destination is a dialog or a share sheet
/// passes an app-owned directory and hands the finished file on from there.
#[uniffi::export]
pub fn build_mail_export_machine_with_key_custody(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    node_url: String,
    handle: String,
    save_dir: String,
) -> Result<Arc<MailExportMachine>, FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    Ok(Arc::new(
        fauna_client_mail_settings::rpc_glue::build_mail_export_machine(
            nest.nest_arc(),
            keypair,
            crate::account_runtime::mail_store(),
            &node_url,
            &handle,
            std::path::PathBuf::from(save_dir),
        ),
    ))
}

/// Build a [`MailImportMachine`] for the user-facing `mail-import` wizard over
/// `nest`'s WS-RPC connection (`mailbox-migration.md` § UX shape) — the export
/// twin's mirror image, and the ONE piece the four native app legs (android,
/// ios, macos, windows) could not supply for themselves: every other part of
/// this page's FFI surface already generates from the shared crate's own
/// `#[uniffi::export]`s (the machine's `snapshot`/`hydrate`/`dispatch`/
/// `run_import`, `importSourceKindLabel`, `importTlsModeLabel`), because only
/// the *builder* needs `FfiNestClient`.
///
/// ⚠ Unlike the export twin, **both seams behind this one are real**: the nest
/// half wraps `MailImportClient<Arc<NestClient>>`, and the source half opens a
/// live `ImapSession` over TCP + TLS against the foreign server (§ Where the
/// IMAP client runs). So a rejection an app surfaces here is a genuine nest or
/// source answer, never an unbuilt-backend explanation — and a successful
/// `Start`/`Resume` obliges the app to spawn [`MailImportMachine::run_import`]
/// once, which is what actually moves messages (the shared machine deliberately
/// does not self-spawn it; tui and linux each do this in their own runtime).
#[uniffi::export]
pub fn build_mail_import_machine(nest: Arc<FfiNestClient>) -> Arc<MailImportMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_mail_import_machine(nest.nest_arc()))
}

/// Build a [`MailSpamMachine`] for the user-facing `mail-spam` page over `nest`'s
/// WS-RPC connection — a person's own per-account spam classifier (`mail-spam.md`
/// § Reset / § Cold start Path 2 / § Undo). The list / reset / baseline toggle are
/// User-class (caller-scoped), but **undo of a client-written (sealed) row** runs
/// the reseal loop, which needs the actor's MSEK — so like
/// [`build_mail_settings_machine`] this takes the actor's 32-byte ed25519 `secret`
/// (to derive the MSEK-sealing keypair) and the deployment `node_url`. Returns
/// `Result` because the keypair derivation is fallible.
#[uniffi::export]
pub fn build_mail_spam_machine(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    node_url: String,
) -> Result<Arc<MailSpamMachine>, FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    Ok(Arc::new(
        fauna_client_mail_settings::rpc_glue::build_mail_spam_machine(
            nest.nest_arc(),
            keypair,
            crate::account_runtime::mail_store(),
            &node_url,
            crate::ledger_seam(),
        ),
    ))
}

/// Generate a random `a-zA-Z0-9` bridge password (the "Auto-generate" default on
/// the `mail-add-credential` dialog). Returned as a plain `String` because it is
/// shown-once for the user to copy into their MUA. See
/// `docs/goal/behavior/mail-credentials.md` § Auto-generated bridge password.
#[uniffi::export]
pub fn generate_bridge_password() -> String {
    fauna_client_mail_settings::password_gen::generate_bridge_password().into()
}

/// Mint a fresh OAUTHBEARER credential token (32 random bytes → 64 lowercase hex
/// chars) for the `mail-add-credential` dialog's bearer-token kind. Returned as a
/// plain `String` because it is shown-once for the user to copy into their MUA.
/// See `docs/goal/behavior/mail-credentials.md` § OAUTHBEARER.
#[uniffi::export]
pub fn generate_bridge_token() -> String {
    fauna_client_mail_settings::password_gen::generate_bridge_token().into()
}

/// Whether to warn that a manually-typed password weakens at-rest: true only when
/// auto-generate is OFF on an encrypted nest (a plaintext nest's data already
/// rests unencrypted, so no incremental warning).
#[uniffi::export]
pub fn warn_manual_bridge_password(auto_generate: bool, nest_encrypted: bool) -> bool {
    fauna_client_mail_settings::password_gen::warn_manual_password(auto_generate, nest_encrypted)
}

/// Resolve the PLAIN-form password field at a settled toggle edge (kind-select
/// → PLAIN, or the auto-generate toggle) — `Some` a freshly-minted secret when
/// `kind` is `Plain` and `auto_generate` is on, `None` (clear the field for
/// manual entry) otherwise. Call ONLY at that edge, never again at submit —
/// store the returned value and submit it verbatim. See
/// `fauna_client_mail_settings::password_gen::resolve_autogenerated_password`.
#[uniffi::export]
pub fn resolve_autogenerated_bridge_password(
    kind: CredentialKind,
    auto_generate: bool,
) -> Option<String> {
    fauna_client_mail_settings::password_gen::resolve_autogenerated_password(kind, auto_generate)
        .map(|s| s.into())
}

/// Substitute the logged-in `handle` into a `MailCredentialSummary.mua_username`
/// template (which carries `{handle}` as its only remaining placeholder), yielding
/// the exact `local@domain` a third-party MUA configures. Native apps
/// (Swift/Kotlin/C#) call this on each `mail-settings` credential row instead of
/// hand-rolling a `replace` that could diverge (priority #2); the bug-prone parts
/// (the `+<credential_id>` suffix, the `default`→bare rule, the domain) are already
/// baked into the shared `mua_username` field. See
/// `docs/goal/behavior/mail-credentials.md` § MUA setup conventions.
#[uniffi::export]
pub fn resolve_mua_username(mua_username: String, handle: String) -> String {
    fauna_client_mail_settings::resolve_mua_username(&mua_username, &handle)
}
