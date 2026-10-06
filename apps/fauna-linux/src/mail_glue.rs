//! Linux glue for the shared mail-admin state machines.
//!
//! The mail-admin pages (`settings/mail.rs` and the admin pages) are dumb
//! renderers of their machine snapshots + dispatchers of their actions; *all*
//! mail logic lives in `libs/fauna-client-mail-settings` + `libs/fauna-client-dns`
//! (priority #2/#4). Every machine is constructed via the shared
//! `rpc_glue::build_*_machine` seam constructors — the per-app
//! `NestClient` / `IdentitySigner` (and admin `…Nest`) seam
//! impls are identical on every native app (they wrap the same
//! `Arc<NestClient>` WS-RPC handle), so they live once in shared Rust and are
//! reused by `fauna-ffi` (windows/macos/ios/android) + `fauna-wasm` (web). Linux
//! only adapts its `FaunaClient` into the constructors' arguments.
//!
//! Everything is WS-RPC; there is no HTTP on any of these paths.
//!
//! There is **no** `MlsSnapshotProvider` seam: the v1 read-side snapshot is
//! pure shared Rust derived from MSEK (`build_mls_snapshot_plaintext`), built
//! inside the machine itself, so the recipient-mail keypair is fleet-consistent
//! by construction. See `docs/goal/architecture/key-material-hierarchy.md`
//! § Path B-sibling-2.

use fauna_client_mail_settings::{
    CaldavPolicyMachine, CarddavPolicyMachine, MailAliasesMachine, MailExportMachine,
    MailImportMachine, MailListMembersMachine, MailListsMachine, MailPolicyMachine,
    MailSettingsMachine, MailSpamMachine, WebdavPolicyMachine,
};
use fauna_core::identity::ActorKeypair;

use crate::client::FaunaClient;

/// Build a `MailSettingsMachine` for the authenticated actor. Derives the
/// actor's keypair from the launch-validated `secret_hex` and delegates the
/// seam wiring (the mail custody / `NestClient` /
/// `IdentitySigner` + the MUA connection-detail block) to the shared `rpc_glue`
/// constructor — the seam
/// impls are no longer per-app (lifted to shared Rust, priority #2/#4). The
/// page calls this on mount, hydrates the snapshot, and renders it.
///
/// Returns `Err` only on an unrecoverable identity fault (the launch flow has
/// already validated `secret_hex` to reach Online, so this is a programmer
/// error in practice).
pub fn build_mail_settings_machine(client: &FaunaClient) -> Result<MailSettingsMachine, String> {
    let keypair = ActorKeypair::from_secret_hex(client.secret_hex())
        .map_err(|e| format!("decode secret_hex: {e}"))?;

    Ok(
        fauna_client_mail_settings::rpc_glue::build_mail_settings_machine(
            client.nest_rpc().clone(),
            keypair,
            crate::account_runtime::mail_store(),
            client.node_url(),
            crate::account_runtime::ledger_seam(),
            Some(std::sync::Arc::new(
                fauna_client_folders::CustodyServedSets(crate::account_runtime::folder_key_store()),
            )),
        ),
    )
}

/// Build the user-settings **Nests** machine wired with *both* the mail
/// relay-provisioning post-link hook *and* the trust facet — the v1 nest-trust
/// surface (`docs/goal/ui/nests.md`). The mail hook auto-provisions the
/// just-linked home box's mailbox on a both-ends `LinkBoth` reusing the fleet
/// MSEK (the one-action home-with-public-relay flow,
/// `deployment-home-with-public-relay.md` § Pairing), so a non-technical user
/// links once and relayed mail is immediately readable there with no separate
/// "enable mail" step; the trust facet hydrates the home nest's grants (a nest is
/// *trusted to read*) + grant-event history and drives Renew/Revoke/SetLens. All
/// logic is shared Rust (priority #2); linux only adapts its `FaunaClient` — the
/// keypair from the launch-validated `secret_hex` and the connected nest handle.
/// Returns `Err` only on an unrecoverable identity fault (the launch flow has
/// validated `secret_hex` to reach Online); the caller falls back to the plain
/// machine so the page still lists / links / unlinks (without the trust facet).
pub fn build_linked_nests_machine_with_mail_relay_and_trust(
    client: &FaunaClient,
) -> Result<fauna_client_pair::LinkedNestsMachine, String> {
    let keypair = ActorKeypair::from_secret_hex(client.secret_hex())
        .map_err(|e| format!("decode secret_hex: {e}"))?;
    // The set names a web-serve paywall grant's folder is named from — folder
    // custody plus the owner's folder list over the same connection.
    let folder_names = fauna_client_pair::TrustFolderNames::new(
        &keypair,
        std::sync::Arc::new(fauna_client_folders::CustodyOwnedSetNames {
            keys: crate::account_runtime::folder_key_store(),
            nest: client.nest_rpc().clone(),
        }),
    );
    Ok(
        fauna_client_mail_settings::rpc_glue::build_linked_nests_machine_with_mail_relay_and_trust(
            client.nest_rpc().clone(),
            keypair,
            crate::account_runtime::ledger_seam(),
            crate::account_runtime::backup_seam(),
            // The blessing door: the account plane's `fauna.state.blessed-nests`
            // over this process's runtime handle (the shared impl).
            std::sync::Arc::new(
                fauna_client_account_runtime::blessed_nests::PlaneBlessedNests::new(
                    crate::account_runtime::handle,
                ),
            ),
            crate::account_runtime::period_key_store(),
            crate::account_runtime::mail_store(),
        )
        .with_folder_names(folder_names),
    )
}

/// Build a `MailAliasesMachine` for the authenticated actor (the user-facing
/// `mail-aliases` page). Unlike the mail-settings machine this needs **no**
/// keypair — the User-class alias RPCs derive the owning actor from the
/// authenticated WS-RPC caller — so it's a thin `FaunaClient` → `Arc<NestClient>`
/// adaptation over the shared `rpc_glue` constructor (priority #2/#4: the seam
/// glue is shared Rust; linux only adapts its client handle).
pub fn build_mail_aliases_machine(client: &FaunaClient) -> MailAliasesMachine {
    fauna_client_mail_settings::rpc_glue::build_mail_aliases_machine(client.nest_rpc().clone())
}

/// Build a `MailSpamMachine` for the authenticated actor (the user-facing
/// `mail-spam` page). The training-history list + reset + baseline toggle are
/// user-tier (the caller-scoped RPCs derive the owning actor from the
/// authenticated WS-RPC connection), but **undo of a client-written (sealed) row**
/// runs the reseal loop, which needs the actor's MSEK — so the shared constructor
/// now carries a `MailSettingsMachine` sealed-model writer built from the same
/// keypair (`mail-spam.md` § Encrypted-mode interaction). The keypair comes from
/// the launch-validated `secret_hex` (`Err` only on an unrecoverable identity
/// fault, like `build_mail_settings_machine`).
pub fn build_mail_spam_machine(client: &FaunaClient) -> Result<MailSpamMachine, String> {
    let keypair = ActorKeypair::from_secret_hex(client.secret_hex())
        .map_err(|e| format!("decode secret_hex: {e}"))?;
    Ok(
        fauna_client_mail_settings::rpc_glue::build_mail_spam_machine(
            client.nest_rpc().clone(),
            keypair,
            crate::account_runtime::mail_store(),
            client.node_url(),
            crate::account_runtime::ledger_seam(),
        ),
    )
}

/// Build a `MailExportMachine` for the authenticated actor (the user-facing
/// `mail-export` wizard) — **with key custody**, because the page spawns the
/// drive loop (`settings/mail_export.rs::after_snapshot`). tui's
/// `mail_glue::build_mail_export_machine` is the twin; the keypair is
/// load-bearing for the same two reasons (the loop opens every record under the
/// actor's standing key set and wraps the per-session blob key under the actor's
/// key — `mail-export.md` § Key material).
///
/// **Custody and the spawn are one change, never two**, so an undecodable
/// secret or a missing save directory degrades to the custody-less machine, not
/// to custody without a spawn: listing, Cancel and Discard keep working and
/// `Start` refuses honestly before any session exists.
///
/// The handle is whatever the settings layer knows now and may be empty on a
/// fresh sign-in; the page refreshes it through `set_actor_handle` before every
/// Start / Resume / Download (the machine's `actor_handle` doc).
pub fn build_mail_export_machine(client: &FaunaClient) -> MailExportMachine {
    let nest = client.nest_rpc().clone();
    let keypair = match ActorKeypair::from_secret_hex(client.secret_hex()) {
        Ok(keypair) => keypair,
        Err(e) => {
            tracing::error!("[mail_glue] export machine: decode secret_hex: {e}");
            return fauna_client_mail_settings::rpc_glue::build_mail_export_machine_without_key_custody(nest);
        }
    };
    let Some(save_dir) = export_save_dir() else {
        tracing::error!("[mail_glue] export machine: no downloads directory to save into");
        return fauna_client_mail_settings::rpc_glue::build_mail_export_machine_without_key_custody(
            nest,
        );
    };
    fauna_client_mail_settings::rpc_glue::build_mail_export_machine(
        nest,
        keypair,
        crate::account_runtime::mail_store(),
        client.node_url(),
        &crate::settings::get_handle().unwrap_or_default(),
        save_dir,
    )
}

/// Where a downloaded mailbox archive lands (§ Download flow step 5): the
/// user's XDG downloads directory — the desktop destination the shared
/// `ExportArchiveDelivery` names, and tui's `backups::download_dir` resolution —
/// with the Done summary saying exactly where (`mail_export.saved_summary_fmt`).
fn export_save_dir() -> Option<std::path::PathBuf> {
    // The e2e harness override — the `FAUNA_E2E_DOWNLOAD_DIR` seam
    // `settings/account.rs`'s export and `views/backups/file_list.rs` use, behind
    // the same two gates (convention 15: compiled out of a release build, and
    // off unless e2e mode is on in a test-capable one).
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    if crate::e2e_mode_enabled()
        && let Some(dir) = std::env::var_os("FAUNA_E2E_DOWNLOAD_DIR").filter(|v| !v.is_empty())
    {
        return Some(std::path::PathBuf::from(dir));
    }
    glib::user_special_dir(glib::UserDirectory::Downloads).or_else(|| Some(glib::home_dir()))
}

/// Build a `MailImportMachine` for the authenticated actor (the user-facing
/// `mail-import` wizard). Like the export twin it needs **no** keypair — the
/// user-tier import RPCs derive the owning actor from the authenticated WS-RPC
/// caller — so it's the same thin `FaunaClient` → `Arc<NestClient>` adaptation
/// over the shared `rpc_glue` constructor (priority #2/#4).
///
/// Unlike export's, **both** seams behind this one are real: the nest half
/// (S9.4) wraps `fauna_mail::imap_client::MailImportClient`, and the source
/// half opens a live `ImapSession` over TCP + TLS against the foreign server
/// (`mailbox-migration.md` § Where the IMAP client runs). Every rejection the
/// page surfaces is therefore a genuine nest/source answer, not an
/// unbuilt-backend explanation.
pub fn build_mail_import_machine(client: &FaunaClient) -> MailImportMachine {
    fauna_client_mail_settings::rpc_glue::build_mail_import_machine(client.nest_rpc().clone())
}

/// Build a `MailListsMachine` for the authenticated actor (the user-facing
/// `mail-lists` page). User-tier (no keypair), thin `FaunaClient` →
/// `Arc<NestClient>` adaptation over the shared `rpc_glue` constructor (priority
/// #2/#4). The list backend is unbuilt (`mail-mass-mailing.md` § Implementation
/// status today), so the shared seam is a stub returning an honest `unimplemented`
/// rejection the page surfaces honestly.
pub fn build_mail_lists_machine(client: &FaunaClient) -> MailListsMachine {
    fauna_client_mail_settings::rpc_glue::build_mail_lists_machine(client.nest_rpc().clone())
}

/// Build a `MailListMembersMachine` for one list (the `mail-list-members` page),
/// scoped to `list_id_hex` (from a rendered list row) with `list_name` as the
/// page heading. Errors only on a malformed `list_id_hex`. The list backend is
/// unbuilt; the shared seam is a stub.
pub fn build_mail_list_members_machine(
    client: &FaunaClient,
    list_id_hex: String,
    list_name: String,
) -> Result<MailListMembersMachine, String> {
    fauna_client_mail_settings::rpc_glue::build_mail_list_members_machine(
        client.nest_rpc().clone(),
        list_id_hex,
        list_name,
    )
    .map_err(|e| e.to_string())
}

/// Build a `MailPolicyMachine` for the flat `admin-mail` policy page (the
/// admin-tier mail-policy form). **Admin-class** — like the bridge-approval /
/// local-domains machines it wraps `MailAdminClient` and needs **no** keypair
/// (the Admin RPCs derive the caller from the authenticated WS-RPC session), so
/// it's a thin `FaunaClient` → `Arc<NestClient>` adaptation over the shared
/// `rpc_glue` constructor (priority #2/#4). The page hydrates via the admin read
/// twin `fauna.bridges.get_mail_config` and saves via `set_mail_enabled` +
/// `put_{spam,auth}_policy`. Both read + write are LIVE (not the honest-
/// `unimplemented` stub the unbuilt user-page backends use). See
/// `docs/goal/behavior/admin.md` § 6 Mail + `mail-policy-config.md`.
pub fn build_mail_policy_machine(client: &FaunaClient) -> MailPolicyMachine {
    fauna_client_mail_settings::rpc_glue::build_mail_policy_machine(client.nest_rpc().clone())
}

/// Build a `CaldavPolicyMachine` for the flat `admin-calendar` page — the
/// CalDAV-enable sibling of `build_mail_policy_machine`. **Admin-class** (wraps
/// `MailAdminClient`, no keypair). The toggle hydrates `caldav_enabled` from the
/// admin read twin `fauna.bridges.get_mail_config` and saves via
/// `fauna.bridges.set_caldav_enabled` (both live; no nest work — the toggle + MDA
/// gate already existed). See `docs/goal/behavior/admin.md` § 8 Calendar +
/// `caldav-server.md` § Independent enablement.
pub fn build_caldav_policy_machine(client: &FaunaClient) -> CaldavPolicyMachine {
    fauna_client_mail_settings::rpc_glue::build_caldav_policy_machine(client.nest_rpc().clone())
}

/// Build a `CarddavPolicyMachine` for the flat `admin-contacts` page — the
/// CardDAV-enable contacts sibling of `build_caldav_policy_machine`.
/// **Admin-class** (wraps `MailAdminClient`, no keypair). The toggle hydrates
/// `carddav_enabled` from the admin read twin `fauna.bridges.get_mail_config`
/// and saves via `fauna.bridges.set_carddav_enabled` (both live; no nest work —
/// the toggle + MDA gate landed in slice 1). See
/// `docs/goal/behavior/admin.md` § Contacts + `carddav-server.md` § Independent
/// enablement.
pub fn build_carddav_policy_machine(client: &FaunaClient) -> CarddavPolicyMachine {
    fauna_client_mail_settings::rpc_glue::build_carddav_policy_machine(client.nest_rpc().clone())
}

/// Build a `WebdavPolicyMachine` for the flat `admin-files` page — the
/// WebDAV-enable files sibling of `build_carddav_policy_machine`.
/// **Admin-class** (wraps `MailAdminClient`, no keypair). The toggle hydrates
/// `webdav_enabled` from the admin read twin `fauna.bridges.get_mail_config`
/// and saves via `fauna.bridges.set_webdav_enabled` (both live; no nest work —
/// the toggle + MDA gate landed in slice 1). See
/// `docs/goal/behavior/admin.md` § Files + `webdav-server.md` § Independent
/// enablement.
pub fn build_webdav_policy_machine(client: &FaunaClient) -> WebdavPolicyMachine {
    fauna_client_mail_settings::rpc_glue::build_webdav_policy_machine(client.nest_rpc().clone())
}

// The admin LocalDomain / BridgeApproval + DNS machines are constructed via the
// same shared `rpc_glue` / `fauna_client_dns` seam constructors (the `…Nest`
// seam glue over `MailAdminClient` / `DnsAdminClient<Arc<NestClient>>` is
// identical on every native app). Re-exported so `client.rs`'s
// `crate::mail_glue::build_*` call sites stay unchanged. The
// `admin-mail-deliverability` page was dropped (DKIM/TLS auto-provisioned, no
// manual UI — see `docs/goal/behavior/dns-management.md`), and its dead
// `DeliverabilityMachine` was removed entirely.
//
// The `admin-dns` page uses the **managed-mode** DNS machine
// (`build_dns_management_machine_with_credentials`), a superset of the
// read/verify-only variant: it additionally wires the client-held credential
// store (the account's `fauna.state.dns` row, through the account runtime) +
// the `fauna-provisioning` publish seam, all in shared Rust from `(nest,
// keypair, account)` — so it loads
// `DnsSnapshot.credentials` + the effective per-domain mode alongside the record
// matrix. `client.rs` derives the keypair (`secret_to_keypair`) and calls this
// in its `fetch_dns_records` / `dns_*` spawns.
pub use fauna_client_dns::build_dns_management_machine_with_credentials;
pub use fauna_client_mail_settings::rpc_glue::{
    build_bridge_approval_machine, build_forwarders_machine, build_local_domains_machine,
};

/// Build an `AtprotoSettingsMachine` for the authenticated actor (the
/// user-facing `atproto` page). Needs a keypair — like mail-settings,
/// unlike labeler-catalog — because the machine signs the D10 delegation with
/// it and its rotation-key custody rests on the account plane (`fauna.state.atproto-identity`);
/// the minted app-credential secrets rest on the account plane
/// (`fauna.state.atproto`, atproto-pds-full.md § D3), wired below.
/// Returns `Err` only on an unrecoverable identity fault (the launch flow has
/// already validated `secret_hex` to reach Online).
pub fn build_atproto_settings_machine(
    client: &FaunaClient,
    observer: std::sync::Arc<dyn fauna_atproto_settings_machine::AtprotoSettingsObserver>,
) -> Result<std::sync::Arc<fauna_atproto_settings_machine::AtprotoSettingsMachine>, String> {
    let keypair = ActorKeypair::from_secret_hex(client.secret_hex())
        .map_err(|e| format!("decode secret_hex: {e}"))?;
    let consent_grants = consent_grant_seams(&keypair);
    let machine = fauna_atproto_settings_machine::build_atproto_settings_machine(
        client.nest_rpc().clone(),
        keypair,
        observer,
        // The S4-C custody alarm posts to the app-wide banner registry.
        Some(crate::critical_alerts::registry()),
    );
    // The ATProto identity custody door (`fauna.state.atproto-identity`)
    // over this process's runtime handle, read fresh per call.
    machine.set_identity_store(std::sync::Arc::new(
        fauna_client_account_runtime::atproto_identity::RuntimeAtprotoIdentity::new(
            crate::account_runtime::handle,
        ),
    ));
    // Where the minted secrets rest: the account plane's `fauna.state.atproto`,
    // through the shared seam over this process's handle, read fresh per call.
    machine.set_credential_store(std::sync::Arc::new(
        fauna_client_account_runtime::atproto_credentials::RuntimeAtprotoCredentials::new(
            crate::account_runtime::handle,
        ),
    ));
    machine.set_consent_grant_seams(consent_grants);
    Ok(machine)
}

/// The consent-time grant's seams (`fauna_atproto_settings_machine::
/// consent_grant`) over this process's runtime handle, read fresh per call —
/// shared by the AT Protocol page's machine and the Connected apps tray's, so
/// either answers a third-party app's records request.
pub fn consent_grant_seams(
    keypair: &ActorKeypair,
) -> std::sync::Arc<fauna_atproto_settings_machine::ConsentGrantSeams> {
    let door = std::sync::Arc::new(fauna_client_config::ResolvingLedgerStore::new(
        crate::account_runtime::handle,
    ));
    std::sync::Arc::new(
        fauna_atproto_settings_machine::ConsentGrantSeams::from_keypair(
            keypair,
            door.clone(),
            door,
        ),
    )
}
