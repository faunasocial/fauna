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
    CredentialKind, DispatchError, DmarcCountRow, DmarcForensicMode, DmarcOpenedReport,
    DmarcPolicyView, DmarcReportRowView, DmarcReportsAction, DmarcReportsMachine,
    DmarcReportsSnapshot, DmarcReportsStatus, DmarcWindowTotals, ExportFormat, ExportScope,
    ExportSessionState, ExportSessionView, ExportStatus, ExportStep, ForwarderAction,
    ForwarderMachine, ForwarderStatus, ForwarderView, ForwardersSnapshot, ImapPolicyView,
    ImportResult, ImportSessionState, ImportSourceKind, ImportStatus, ImportStep, ImportTlsMode,
    ImportTotals, ListDraft, ListMembers, ListView, ListsStatus, LocalDomainAction,
    LocalDomainMachine, LocalDomainStatus, LocalDomainView, LocalDomainsSnapshot,
    MailAliasesAction, MailAliasesMachine, MailAliasesSnapshot, MailCredentialSummary,
    MailExportAction, MailExportMachine, MailExportSnapshot, MailImportAction, MailImportMachine,
    MailImportSnapshot, MailListMembersAction, MailListMembersMachine, MailListMembersSnapshot,
    MailListsAction, MailListsMachine, MailListsSnapshot, MailPolicyAction, MailPolicyMachine,
    MailPolicySnapshot, MailPolicyStatus, MailSettingsAction, MailSettingsMachine,
    MailSettingsSnapshot, MailSpamAction, MailSpamMachine, MailSpamSnapshot, MailboxOption,
    MailboxProgressView, MemberStatus, MemberView, MuaInstructions, NestError, OutboundPolicyView,
    PendingBridgeView, PendingRotationStatus, PrimaryDomainRenameView, SettingsStatus,
    SourceMailboxOption, SpamHistory, SpamPolicyView, SpamStatus, SpamTrainingView,
    SubmissionPolicyView, TrainingLabel, TrainingSource, WebdavPolicyAction, WebdavPolicyMachine,
    WebdavPolicySnapshot, WebdavPolicyStatus,
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

/// Seal the DNS-provider credential the onboarding wizard captured
/// (`OnboardingMachine::captured_dns_credential`) into the admin's
/// `fauna.state.dns` — the ONE call an app's `LoggedIn` hand-off makes
/// (`onboarding-provisioning.md` § 4. DNS configuration), the UniFFI face of
/// `fauna_client_dns::dispatch_seal_captured_dns_credential` so apple and
/// android are call sites rather than a fourth and fifth dialect of the
/// build-machine → map-fields → dispatch body. Best-effort and log-only: it
/// resolves once the dispatch has answered and errs only on an undecodable
/// `secret`; a seal failure is logged and the credential stays re-enterable
/// from the admin DNS page.
#[fauna_uniffi_async::export]
pub async fn seal_captured_dns_credential(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    credential: fauna_onboarding_machine::CapturedDnsCredential,
) -> Result<(), FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    fauna_client_dns::dispatch_seal_captured_dns_credential(
        nest.nest_arc(),
        keypair,
        Arc::new(crate::account_runtime::handle),
        credential.provider_id,
        credential.fields,
        credential.label,
    )
    .await;
    Ok(())
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

/// Build a [`DmarcReportsMachine`] for the `admin-mail-reports` page — the DMARC
/// aggregate reports peers send about this nest's domains (dmarc-reporting.md
/// § Admin pane summary surface) — over `nest`'s WS-RPC connection. Admin
/// machine: `list_dmarc_aggregate_reports` / `get_dmarc_aggregate_report` /
/// `list_local_domains` are Admin-class, no actor secret.
#[uniffi::export]
pub fn build_dmarc_reports_machine(nest: Arc<FfiNestClient>) -> Arc<DmarcReportsMachine> {
    Arc::new(fauna_client_mail_settings::rpc_glue::build_dmarc_reports_machine(nest.nest_arc()))
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

/// Start the mail connect-time refresh on `nest`'s connection: once now and
/// again after every reconnect of it, for the client's life
/// (`mail-credentials.md` § Hard revoke → *Published before the nest moves*,
/// rule (4)) — the shared
/// `fauna_client_mail_settings::rpc_glue::refresh_epoch_schedule_on_every_connect`
/// tui and linux spawn too. Call it ONCE per signed-in client, at the
/// post-auth edge; it replaces an app's own connect and reconnect legs. Returns
/// at once (the loop runs on a runtime worker and ends when the client drops);
/// best-effort and log-only.
#[fauna_uniffi_async::export]
pub async fn start_mail_epoch_refresh_on_every_connect(nest: Arc<FfiNestClient>) {
    tokio::spawn(
        fauna_client_mail_settings::rpc_glue::refresh_epoch_schedule_on_every_connect(
            &nest.nest_arc(),
            crate::account_runtime::mail_store(),
        ),
    );
}

/// Set the submission-token refresh's clock offset
/// (`fauna_client_mail_settings::token_refresh::set_clock_offset_secs`) — the
/// `mail_token_refresh_advance_clock` agent command's mechanism, moving the
/// `now` the 7-day re-mint cadence is measured against and each re-minted
/// token's `issued_at` (never the bearer clock) without sleeping out the
/// cadence (testing.md convention 14). The loop
/// [`start_mail_epoch_refresh_on_every_connect`] starts reads it on its next
/// pass. Gated on `test-helpers` so the symbol is absent from a release binding
/// (convention 15); `test-helpers` forwards the mail-settings crate's
/// `e2e-agent` so the setter exists in the release-profile `*-ffi-test` builds.
/// Mirrors `set_trust_clock_offset_secs`.
///
/// ⚠ Process-wide, and nothing auto-resets it — zero it once the assertions
/// are done.
#[cfg(feature = "test-helpers")]
#[uniffi::export]
pub fn set_mail_token_refresh_clock_offset_secs(offset_secs: i64) {
    fauna_client_mail_settings::token_refresh::set_clock_offset_secs(offset_secs);
}

/// The post-claim serving enablement (`onboarding.md` § 3b *Mechanism*) — the
/// ONE call an app's `LoggedIn` handoff makes with the four intents it read off
/// the onboarding machine (`email_enable_requested` and siblings), replacing
/// the per-app firing of the mail provision, the three DAV toggles and their
/// companion mints. Runs the shared
/// `fauna_client_mail_settings::serving_enablement` plan to the end over
/// `nest`'s connection and resolves once every step has succeeded or been
/// refused for good (a passing failure is tried again inside — `onboarding.md`
/// § 3b *The step completes; it is not fired once*). Returns the page's
/// `error-message` text when a step was refused for good, else `None`; errs
/// only on an undecodable `secret`. Progress is [`serving_enablement_json`].
#[fauna_uniffi_async::export]
pub async fn apply_post_claim_serving_enablement(
    nest: Arc<FfiNestClient>,
    secret: Vec<u8>,
    node_url: String,
    email: bool,
    caldav: bool,
    carddav: bool,
    webdav: bool,
) -> Result<Option<String>, FfiError> {
    let keypair = crate::keypair_from_bytes(&secret)?;
    let report = fauna_client_mail_settings::rpc_glue::dispatch_post_claim_serving_enablement(
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
    Ok(report.error_message())
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
    Arc::new(
        fauna_client_mail_settings::rpc_glue::build_mail_import_machine(nest.nest_arc())
            .with_source_memory(crate::account_runtime::mail_import_source_store()),
    )
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

/// The `admin-dns` served-cert badge for one [`CertStatusRow`] — `fauna_core`'s
/// `cert_status_view` fed straight from the row's typed `state`, so Swift and
/// Kotlin no longer hand-map `CertHealthState` to its serde variant name before
/// calling the string-keyed `value_format::cert_status_view` (the same
/// enum→string table, hand-written once per app and pinned against nothing; the
/// name comes from [`CertHealthState::as_str`], which a unit test pins to serde).
/// See `docs/goal/behavior/value-formatting.md` § Cert status badge.
#[uniffi::export]
pub fn cert_row_status_view(row: CertStatusRow) -> fauna_core::format::CertStatusView {
    fauna_core::format::cert_status_view(row.state.as_str(), row.is_floor, row.not_after_unix)
}

// ── admin-dns `Possibility` faces (`docs/goal/ui/README.md` § Offer only what
// is possible; the ruling is `docs/goal/behavior/dns-management.md` § Errors &
// edge cases) ───────────────────────────────────────────────────────────────
//
// Each forwards one shared answer untouched (`fauna_client_dns::affordances`,
// `fauna_client_mail_settings::domain_forms`), so Kotlin, Swift and C# paint
// the `admin-dns` controls from the same answer tui reads in Rust and refuse
// the action with the same reason. The snapshots are `uniffi::Record`s and so
// cannot carry an exported method; they cross by value, the app passing the
// one it already holds (`None` = that read has not resolved). Gated
// `value-format` too, for the bare-`fauna_core`-type reason
// [`crate::offline::offline_possibility`] gives.

/// `admin-dns-domain-mode` on `domain`, switching to `managed`:
/// [`DnsSnapshot::set_mode_possibility`]. `dns` `None` = the DNS read has not
/// resolved, never possible.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn dns_set_mode_possibility(
    dns: Option<DnsSnapshot>,
    domain: String,
    managed: bool,
) -> fauna_core::possibility::Possibility {
    match dns {
        Some(dns) => dns.set_mode_possibility(&domain, managed),
        None => fauna_core::possibility::Possibility::once_read(false),
    }
}

/// `admin-dns-manage-all-toggle`, switching to `managed`:
/// [`fauna_client_dns::affordances::manage_all_possibility`] over the DNS read
/// and the active domain names (`None` = that read has not resolved).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn dns_manage_all_possibility(
    dns: Option<DnsSnapshot>,
    active_domains: Option<Vec<String>>,
    managed: bool,
) -> fauna_core::possibility::Possibility {
    fauna_client_dns::affordances::manage_all_possibility(
        dns.as_ref(),
        active_domains.as_deref(),
        managed,
    )
}

/// `admin-dns-add-credential-submit-button`:
/// [`fauna_client_dns::affordances::credential_draft_possibility`]. `values` is
/// what has been typed into each of the picked provider's DNS fields, in any
/// order (blank when untouched).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn dns_credential_draft_possibility(
    provider_picked: bool,
    values: Vec<String>,
) -> fauna_core::possibility::Possibility {
    let fields: Vec<(&str, &str)> = values.iter().map(|v| ("", v.as_str())).collect();
    fauna_client_dns::affordances::credential_draft_possibility(provider_picked, &fields)
}

/// `admin-dns-cert-issue-button` on `domain`:
/// [`fauna_client_dns::affordances::cert_issue_possibility`] over the
/// snapshot's `pending_cert`.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn dns_cert_issue_possibility(
    pending: Option<PendingCertIssue>,
    domain: String,
) -> fauna_core::possibility::Possibility {
    fauna_client_dns::affordances::cert_issue_possibility(pending.as_ref(), &domain)
}

/// `admin-dns-cert-delegate-button`:
/// [`fauna_client_dns::affordances::delegate_possibility`] — blocked until a
/// held credential covers a zone (`dns-management.md` § Errors & edge cases,
/// *Renewal delegation and the rename banner answer too*).
/// `credential_zones` is every zone the snapshot's credentials cover.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn dns_delegate_possibility(
    credential_zones: Vec<String>,
) -> fauna_core::possibility::Possibility {
    fauna_client_dns::affordances::delegate_possibility(&credential_zones)
}

/// One of the rename banner's state-gated actions.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiPrimaryDomainRenameAction {
    /// `admin-dns-rename-complete-button` and its confirm (force-complete
    /// included).
    Complete,
    /// The grace extension's day input and button.
    Extend,
    /// `admin-dns-rename-abort-button` and its confirm.
    Abort,
}

/// `action` on the rename banner showing `view`:
/// `PrimaryDomainRenameView::{complete,extend,abort}_possibility` — possible
/// in the states that take it, absent outside them (`dns-management.md`
/// § Errors & edge cases, *Renewal delegation and the rename banner answer
/// too*).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn primary_domain_rename_action_possibility(
    view: PrimaryDomainRenameView,
    action: FfiPrimaryDomainRenameAction,
) -> fauna_core::possibility::Possibility {
    match action {
        FfiPrimaryDomainRenameAction::Complete => view.complete_possibility(),
        FfiPrimaryDomainRenameAction::Extend => view.extend_possibility(),
        FfiPrimaryDomainRenameAction::Abort => view.abort_possibility(),
    }
}

/// `admin-mail-enabled-toggle`:
/// [`MailPolicySnapshot::enable_toggle_possibility`] — turning mail on is
/// blocked by the `admin-mail-unavailable-notice` sentence on a box that
/// cannot enable it; mail already on stays possible so it can be turned off
/// (`admin.md` § Errors & edge cases).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn mail_enable_toggle_possibility(
    snapshot: MailPolicySnapshot,
) -> fauna_core::possibility::Possibility {
    snapshot.enable_toggle_possibility()
}

/// `admin-dns-add-domain-submit-button`:
/// [`fauna_client_mail_settings::domain_forms::add_domain_possibility`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn local_domain_add_possibility(
    domains: Option<LocalDomainsSnapshot>,
    draft: String,
) -> fauna_core::possibility::Possibility {
    fauna_client_mail_settings::domain_forms::add_domain_possibility(domains.as_ref(), &draft)
}

/// `admin-dns-domain-remove-button` on `row`:
/// [`fauna_client_mail_settings::domain_forms::remove_possibility`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn local_domain_remove_possibility(
    row: LocalDomainView,
) -> fauna_core::possibility::Possibility {
    fauna_client_mail_settings::domain_forms::remove_possibility(&row)
}

/// `admin-dns-domain-rename-button` (the primary row):
/// [`fauna_client_mail_settings::domain_forms::rename_possibility`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn local_domain_rename_possibility(
    domains: LocalDomainsSnapshot,
) -> fauna_core::possibility::Possibility {
    fauna_client_mail_settings::domain_forms::rename_possibility(&domains)
}

/// `admin-dns-domain-promote-button` (a non-primary row):
/// [`fauna_client_mail_settings::domain_forms::promote_possibility`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn local_domain_promote_possibility(
    domains: LocalDomainsSnapshot,
) -> fauna_core::possibility::Possibility {
    fauna_client_mail_settings::domain_forms::promote_possibility(&domains)
}

/// `admin-dns-rename-submit-button`:
/// [`fauna_client_mail_settings::domain_forms::rename_draft_possibility`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn local_domain_rename_draft_possibility(
    domains: Option<LocalDomainsSnapshot>,
    target: String,
    grace_days: String,
) -> fauna_core::possibility::Possibility {
    fauna_client_mail_settings::domain_forms::rename_draft_possibility(
        domains.as_ref(),
        &target,
        &grace_days,
    )
}

/// `admin-dns-rename-extend-button`:
/// [`fauna_client_mail_settings::domain_forms::extend_draft_possibility`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn local_domain_extend_draft_possibility(days: String) -> fauna_core::possibility::Possibility {
    fauna_client_mail_settings::domain_forms::extend_draft_possibility(&days)
}

// The mail, calendar and aliases forms' draft-validity answers (`admin.md`
// § Errors & edge cases → *A form's submit is live only for a draft the nest
// would accept*): the natives paint each submit from these, folded after the
// page's read gate, and ask again at the press.

/// A mail group's save (`admin.md` § 6 Mail):
/// [`fauna_client_mail_settings::admin_policy::knobs_draft_possibility`] over
/// the group's `u32` drafts, its `u64` drafts and its one-number-per-line
/// lists. A group whose number does not parse is never saved with the stored
/// value; once possible, the values are `parse_count` / `parse_count_u64` of
/// the same drafts.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn mail_knobs_draft_possibility(
    counts: Vec<String>,
    counts_u64: Vec<String>,
    lists: Vec<String>,
) -> fauna_core::possibility::Possibility {
    fauna_client_mail_settings::admin_policy::knobs_draft_possibility(&counts, &counts_u64, &lists)
}

/// `admin-mail-microsoft-save-button`:
/// [`fauna_client_mail_settings::admin_policy::microsoft_draft_possibility`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn mail_microsoft_draft_possibility(
    client_id: String,
    tenant: String,
) -> fauna_core::possibility::Possibility {
    fauna_client_mail_settings::admin_policy::microsoft_draft_possibility(&client_id, &tenant)
}

/// `admin-calendar-caldav-port-save-button`:
/// [`CaldavPolicyMachine::port_draft_possibility`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn caldav_port_draft_possibility(draft: String) -> fauna_core::possibility::Possibility {
    CaldavPolicyMachine::port_draft_possibility(&draft)
}

/// The aliases page's forwarder add:
/// [`ForwarderMachine::create_draft_possibility`].
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn forwarder_create_draft_possibility(
    local_domain: String,
    pattern: String,
    forward_target: String,
) -> fauna_core::possibility::Possibility {
    ForwarderMachine::create_draft_possibility(&local_domain, &pattern, &forward_target)
}

/// The day count a grace or extend draft names:
/// [`fauna_client_mail_settings::domain_forms::parse_rename_days`]'s `Ok`.
/// `None` for a blank draft (the nest's default) — and for an invalid one,
/// which the caller has already refused through the draft's answer above, so
/// an invalid draft is never silently replaced by the default.
#[uniffi::export]
pub fn local_domain_rename_days(draft: String) -> Option<i64> {
    fauna_client_mail_settings::domain_forms::parse_rename_days(&draft)
        .ok()
        .flatten()
}

#[cfg(all(test, feature = "value-format"))]
mod admin_dns_possibility_face_tests {
    use super::*;

    fn key(p: &fauna_core::possibility::Possibility) -> Option<String> {
        p.reason().map(|r| r.key.clone())
    }

    #[test]
    fn an_unread_snapshot_is_still_loading_never_possible() {
        assert_eq!(
            key(&dns_set_mode_possibility(None, "a.test".into(), true)),
            Some("common.still_loading".into())
        );
        assert_eq!(
            key(&dns_manage_all_possibility(None, None, true)),
            Some("common.still_loading".into())
        );
        assert_eq!(
            key(&local_domain_add_possibility(None, "a.test".into())),
            Some("common.still_loading".into())
        );
    }

    #[test]
    fn the_faces_forward_the_shared_answers() {
        assert_eq!(
            key(&dns_credential_draft_possibility(true, vec![" ".into()])),
            Some(fauna_client_dns::affordances::CREDENTIAL_FIELD_REQUIRED.into())
        );
        assert!(dns_credential_draft_possibility(true, vec!["t".into()]).is_possible());
        assert!(dns_cert_issue_possibility(None, "a.test".into()).is_possible());
        assert_eq!(
            key(&local_domain_extend_draft_possibility("31".into())),
            Some(fauna_client_mail_settings::domain_forms::RENAME_DAYS_INVALID.into())
        );
    }

    #[test]
    fn rename_days_reads_blank_as_the_default_and_a_valid_count_as_itself() {
        assert_eq!(local_domain_rename_days(" ".into()), None);
        assert_eq!(local_domain_rename_days("14".into()), Some(14));
        assert_eq!(local_domain_rename_days("99".into()), None);
    }
}

#[cfg(test)]
mod cert_row_status_view_tests {
    use super::*;

    fn row(state: CertHealthState, is_floor: bool, not_after_unix: i64) -> CertStatusRow {
        CertStatusRow {
            domain: "d.example".into(),
            state,
            not_after_unix,
            is_floor,
        }
    }

    #[test]
    fn the_typed_row_matches_the_string_keyed_view_for_every_state() {
        for state in [
            CertHealthState::ValidTrusted,
            CertHealthState::OnFloorRenewNeeded,
            CertHealthState::Expiring,
        ] {
            for is_floor in [false, true] {
                assert_eq!(
                    cert_row_status_view(row(state, is_floor, 2_000_000_000)),
                    fauna_core::format::cert_status_view(state.as_str(), is_floor, 2_000_000_000),
                    "{state:?} floor={is_floor}"
                );
            }
        }
    }
}

#[cfg(all(test, feature = "value-format"))]
mod rename_and_delegate_possibility_face_tests {
    use super::*;
    use fauna_core::possibility::Possibility;

    fn view(complete: bool, force: bool, extend: bool, abort: bool) -> PrimaryDomainRenameView {
        PrimaryDomainRenameView {
            rename_id: vec![0; 16],
            state: "grace".into(),
            old_primary_domain: "old.test".into(),
            new_primary_domain: "new.test".into(),
            started_at: 0,
            grace_days: 7,
            grace_ends_at: None,
            ready_to_complete_at: None,
            is_post_flip_active: true,
            is_pre_flip: false,
            can_complete: complete,
            can_force_complete: force,
            can_extend: extend,
            can_abort: abort,
        }
    }

    #[test]
    fn each_banner_action_answers_its_own_state() {
        use FfiPrimaryDomainRenameAction::*;
        let none = view(false, false, false, false);
        for action in [Complete, Extend, Abort] {
            assert_eq!(
                primary_domain_rename_action_possibility(none.clone(), action),
                Possibility::Absent
            );
        }
        assert_eq!(
            primary_domain_rename_action_possibility(view(false, true, false, false), Complete),
            Possibility::Possible
        );
        assert_eq!(
            primary_domain_rename_action_possibility(view(false, false, true, false), Extend),
            Possibility::Possible
        );
        assert_eq!(
            primary_domain_rename_action_possibility(view(false, false, false, true), Abort),
            Possibility::Possible
        );
    }

    #[test]
    fn delegation_waits_for_a_credential_zone() {
        assert_eq!(
            dns_delegate_possibility(Vec::new())
                .reason()
                .map(|r| r.key.as_str()),
            Some("admin.dns.cert.delegate_no_zones")
        );
        assert_eq!(
            dns_delegate_possibility(vec!["example.test".into()]),
            Possibility::Possible
        );
    }
}
