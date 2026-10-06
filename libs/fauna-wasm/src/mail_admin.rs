//! `#[wasm_bindgen]` exposure of the admin mail/DNS state machines to the
//! Svelte SPA — the WASM twin of the UniFFI exposure in `fauna-ffi/src/mail_admin.rs`
//! (tracked internally, Slice 2). Lets the web `admin-dns` /
//! `admin-bridges-pending` pages drive the shared machines (`libs/fauna-client-dns`,
//! `libs/fauna-client-mail-settings`) instead of re-implementing DNS / domain /
//! bridge-approval logic in TypeScript (priority #2/#3;
//! `docs/goal/behavior/dns-management.md` § Where logic lives: "Per-app —
//! render the snapshot, dispatch actions; no DNS logic in any shell").
//!
//! Each machine is built from the SPA's singleton browser `WsRpcClient` (the
//! `WsRpcClient::*Machine()` factories in `src/rpc.rs`) and holds it through the
//! shared crate's wasm `build_*_machine` seam. The surface mirrors the UniFFI
//! one exactly so the consumer model is identical across all seven apps:
//!   * `snapshot()` — sync, returns the rendered snapshot as a plain JS object
//!     (numbers-as-numbers via `serde_wasm_bindgen::Serializer::json_compatible`).
//!   * `hydrate()` — async (`Promise`), the initial page load.
//!   * `dispatch(action)` — async (`Promise`), `action` a JS object decoded into
//!     the machine's action enum; resolves `undefined` on success (read the new
//!     state with `snapshot()`), rejects with the error string on failure.
//!
//! The machine is `!Send` on wasm (its `Arc<dyn …Nest>` seam wraps the `Rc`-based
//! `WsRpcClient`), so the wrapper holds it in an `Rc` and the dispatch futures run
//! on the wasm-bindgen single-threaded local executor — neither needs `Send`.
//!
//! The whole module is `#[cfg(target_arch = "wasm32")]` (gated at the `mod` site
//! in `lib.rs`), like `src/rpc.rs`: the shared seams' wasm transport
//! (`fauna-rpc-wasm`) only exists on wasm.

use std::rc::Rc;
use std::sync::Arc;

#[cfg(feature = "test-helpers")]
use fauna_client_dns::enable_dns_provider_fake_for_test;
use fauna_client_dns::{
    DnsAction, DnsManagementMachine, build_dns_management_machine,
    build_dns_management_machine_with_credentials,
};
use fauna_client_mail_settings::rpc_glue::{
    build_bridge_approval_machine, build_caldav_policy_machine, build_carddav_policy_machine,
    build_forwarders_machine, build_local_domains_machine, build_mail_aliases_machine,
    build_mail_export_machine, build_mail_import_machine, build_mail_list_members_machine,
    build_mail_lists_machine, build_mail_policy_machine, build_mail_settings_machine,
    build_mail_spam_machine, build_webdav_policy_machine,
};
use fauna_client_mail_settings::{
    BridgeApprovalAction, BridgeApprovalMachine, CaldavPolicyAction, CaldavPolicyMachine,
    CarddavPolicyAction, CarddavPolicyMachine, CredentialKind, ForwarderAction, ForwarderMachine,
    LocalDomainAction, LocalDomainMachine, MailAliasesAction, MailAliasesMachine, MailExportAction,
    MailExportMachine, MailImportAction, MailImportMachine, MailListMembersAction,
    MailListMembersMachine, MailListsAction, MailListsMachine, MailPolicyAction, MailPolicyMachine,
    MailSettingsAction, MailSettingsMachine, MailSpamAction, MailSpamMachine, WebdavPolicyAction,
    WebdavPolicyMachine,
};
use fauna_core::identity::ActorKeypair;
use fauna_rpc_wasm::WsRpcClient as InnerClient;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

use crate::mail_export_delivery::{MailExportSavePort, WebExportArchiveDelivery};
use crate::rpc::{err_to_js, from_js, to_js};

// The wrappers are structurally identical (an `Rc<Machine>` + the same
// snapshot/hydrate/dispatch surface); only the machine type, action type, and
// build seam differ. The shared `wasm_admin_machine!` macro
// (`src/wasm_admin_machine.rs`) keeps them in lockstep.
wasm_admin_machine!(
    /// Web `admin-dns` page — the per-domain DNS record matrix + live red/green
    /// public-DNS verdicts (`fauna.dns.{list_records,verify_records}`).
    WasmDnsManagementMachine,
    DnsManagementMachine,
    DnsAction,
    build_dns_management_machine,
);

/// Test-only: enable the wasm fake DNS provider — the wasm twin of native's
/// `FAUNA_DNS_PROVIDER_FAKE` env gate. After this is called, any
/// `WasmDnsManagementMachine::build_with_credentials` recognizes the
/// `fake-dns-ok:<zone>` sentinel token (verify → those zones, publish → ok, no
/// network), letting a tier_2 e2e drive the managed-publish / onboarding-launch
/// success path with no real registrar reachable from the sandbox.
///
/// Gated on this crate's off-by-default `test-helpers` feature, so it does NOT
/// ship in production wasm. The previous version of this doc comment said the
/// opposite — "this symbol ships in production wasm … so it cannot be `cfg`-gated
/// out" — which was true only because `wasm-core` had no test flavor to build;
/// `wasm-core-test` (2026-07-30) is that flavor, so the stated blocker is gone.
/// Invoked only from the Playwright e2e bridge
/// (`window.__fauna_enableDnsFakeProviderForTest`), never by production code.
///
/// ⚠ Residual, deliberately NOT closed here: the underlying
/// `fauna_client_dns::enable_dns_provider_fake_for_test` and its
/// `SentinelDnsProvider` branch are still ungated in that crate (its `test-helpers`
/// feature is an empty `[]` placeholder). Nothing can reach them on web now that
/// this export is gone, but the rule-(a) visibility gate there is shared with the
/// four native apps' FFI artifact — it belongs with the native-FFI
/// leg, not with a web-only change.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen(js_name = enableDnsFakeProviderForTest)]
pub fn enable_dns_fake_provider_for_test() {
    enable_dns_provider_fake_for_test();
}

/// `fauna_client_mail_settings::local_domains::role_address_options` → the four
/// overridable RFC 2142 role addresses in render order, each entry carrying both
/// its `role_address_overrides` storage key (the `<key>@` caption and the
/// `admin-dns-domain-role-address-<key>-select` id suffix) and the `kind` tag the
/// `SetRoleAddress` action takes.
///
/// The web door for the table linux and tui reach by calling
/// `RoleAddressKind::as_storage_key` directly. Mirrors `contentFloorOptions` /
/// `unknownSenderOptions` — the established shape for handing a shared
/// vocabulary to a client that cannot call the Rust enum (priority #2).
#[wasm_bindgen(js_name = roleAddressOptions)]
pub fn role_address_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_mail_settings::local_domains::role_address_options())
}

/// `fauna_client_mail_settings::admin_policy::fcrdns_mode_options` → the three
/// `admin-mail-fcrdns-mode-select` values in render order, each carrying its wire
/// token and i18n label key.
///
/// The web door for the table tui and linux reach by calling `FcrdnsMode::ORDER`
/// directly. Same shape as `roleAddressOptions` above; the tokens are pinned
/// against the Go MTA's own const family by
/// `libs/fauna-client-mail-settings/tests/go_wire_policy_contract.rs`, because
/// this vocabulary is a *policy value* the bridge switches on as well as a
/// picker's values (priority #2).
#[wasm_bindgen(js_name = fcrdnsModeOptions)]
pub fn fcrdns_mode_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_mail_settings::admin_policy::fcrdns_mode_options())
}

/// `fauna_client_mail_settings::admin_policy::imap_delete_nonempty_options` →
/// the two `admin-mail-imap-delete-nonempty-select` values in render order.
/// Sibling of `fcrdnsModeOptions`; the same page's other raw-value picker.
#[wasm_bindgen(js_name = imapDeleteNonemptyOptions)]
pub fn imap_delete_nonempty_options() -> Result<JsValue, JsValue> {
    crate::rpc::to_js(&fauna_client_mail_settings::admin_policy::imap_delete_nonempty_options())
}

impl WasmDnsManagementMachine {
    /// Build the **managed-mode** variant — adds the client-held DNS-provider
    /// credential store (the account's `fauna.state.dns` row, read and written
    /// through this tab's account runtime) + the publish/verify provider seam on
    /// top of the read/verify surface. The store waits for the runtime when the
    /// tab has not started it yet — the SPA starts it only once the
    /// conversations manager exists, and onboarding's seal-at-capture builds
    /// this machine at `LoggedIn`, before that. Takes the actor's 32-byte
    /// ed25519 `secret` for the issuance seal. Called by the
    /// `WsRpcClient::dnsManagementMachineWithCredentials` factory in `src/rpc.rs`.
    /// The `snapshot`/`hydrate`/`dispatch` surface is the macro's — only the
    /// machine wiring differs, so the `admin-dns` page drives one type whether or
    /// not it holds credentials.
    pub(crate) fn build_with_credentials(
        client: InnerClient,
        secret: Vec<u8>,
    ) -> Result<Self, JsValue> {
        let arr: [u8; 32] = secret
            .try_into()
            .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
        let keypair = ActorKeypair::from_secret(arr);
        Ok(Self {
            inner: Rc::new(build_dns_management_machine_with_credentials(
                client,
                keypair,
                std::sync::Arc::new(crate::account_runtime::handle),
            )),
        })
    }
}

#[wasm_bindgen]
impl WasmDnsManagementMachine {
    /// The deployment "Fauna controls DNS" master-switch state over the current
    /// snapshot — active ⟺ ≥1 active domain and every active domain is effectively
    /// managed. `activeDomains` is the `admin-dns` active set (the
    /// `WasmLocalDomainMachine` snapshot's `active` domain names) the page already
    /// holds; empty ⇒ off. Lets the web `admin-dns` page drop its hand-coded
    /// `allManaged` derived in favour of the shared projection
    /// ([`fauna_client_dns::DnsSnapshot::all_domains_managed`]).
    #[wasm_bindgen(js_name = allDomainsManaged)]
    pub fn all_domains_managed(&self, active_domains: Vec<String>) -> bool {
        self.inner.all_domains_managed(active_domains)
    }
}

wasm_admin_machine!(
    /// Web `admin-dns` page — list / add / remove / restore / update the
    /// mail-hosting domains.
    WasmLocalDomainMachine,
    LocalDomainMachine,
    LocalDomainAction,
    build_local_domains_machine,
);

wasm_admin_machine!(
    /// Web `admin-bridges-pending` page — the pending-bridge approval feed +
    /// deployment-wide mail-enable toggle.
    WasmBridgeApprovalMachine,
    BridgeApprovalMachine,
    BridgeApprovalAction,
    build_bridge_approval_machine,
);

wasm_admin_machine!(
    /// Web flat `admin-mail` policy page — the deployment-wide inbound spam
    /// perimeter + submission AUTH policy form (`admin.md` § Mail /
    /// `mail-policy-config.md` Tier 2/3), backed by the admin read twin
    /// `fauna.bridges.get_mail_config` + the `set_mail_enabled` /
    /// `put_{spam,auth}_policy` writes. The wasm twin of the UniFFI
    /// `build_mail_policy_machine` exposure — an Admin-class machine like
    /// `WasmForwarderMachine`, so the connection-only macro fits. Completes the
    /// two-layer lift so web's gated Bundle B lifts the shared `MailPolicyMachine`
    /// over wasm instead of re-deriving the policy form in TypeScript (priority
    /// #2), parity with the native apps that already drive it over FFI.
    WasmMailPolicyMachine,
    MailPolicyMachine,
    MailPolicyAction,
    build_mail_policy_machine,
);

wasm_admin_machine!(
    /// Web flat `admin-calendar` page — the deployment-wide CalDAV-enable toggle
    /// (`admin.md` § 8 Calendar / `caldav-server.md` § Independent enablement), the
    /// sibling of `WasmMailPolicyMachine`. Hydrates `caldav_enabled` from the admin
    /// read twin `fauna.bridges.get_mail_config` + saves via `set_caldav_enabled`.
    /// The wasm twin of the UniFFI `build_caldav_policy_machine` exposure — an
    /// Admin-class connection-only machine, so the macro fits. Lets web's
    /// `admin/calendar/+page.svelte` lift the shared `CaldavPolicyMachine` over wasm
    /// (priority #2), parity with the native apps driving it over FFI.
    WasmCaldavPolicyMachine,
    CaldavPolicyMachine,
    CaldavPolicyAction,
    build_caldav_policy_machine,
);

wasm_admin_machine!(
    /// Web flat `admin-contacts` page — the deployment-wide CardDAV-enable toggle
    /// (`admin.md` § Contacts / `carddav-server.md` § Independent enablement), the
    /// contacts sibling of `WasmCaldavPolicyMachine`. Hydrates `carddav_enabled`
    /// from the admin read twin `fauna.bridges.get_mail_config` + saves via
    /// `set_carddav_enabled`. The wasm twin of the UniFFI
    /// `build_carddav_policy_machine` exposure — an Admin-class connection-only
    /// machine, so the macro fits.
    WasmCarddavPolicyMachine,
    CarddavPolicyMachine,
    CarddavPolicyAction,
    build_carddav_policy_machine,
);

wasm_admin_machine!(
    /// Web flat `admin-files` page — the deployment-wide WebDAV-enable toggle
    /// (`admin.md` § Files / `webdav-server.md` § Independent enablement), the
    /// files sibling of `WasmCarddavPolicyMachine`. Hydrates `webdav_enabled`
    /// from the admin read twin `fauna.bridges.get_mail_config` + saves via
    /// `set_webdav_enabled`. The wasm twin of the UniFFI
    /// `build_webdav_policy_machine` exposure — an Admin-class connection-only
    /// machine, so the macro fits.
    WasmWebdavPolicyMachine,
    WebdavPolicyMachine,
    WebdavPolicyAction,
    build_webdav_policy_machine,
);

wasm_admin_machine!(
    /// Web `admin-aliases` page (forwarder half) — admin **external forwarders**
    /// (`admin.md` § 4 / `mail-aliases.md` § Kind 7, ratified 2026-06-01): map an
    /// address on a hosted local domain to an external destination with no local
    /// mailbox. Drives the Admin-class `ForwarderMachine`
    /// (`fauna.bridges.{create,list,delete}_forwarder` over `MailAdminClient`),
    /// the wasm twin of the UniFFI exposure. Owner is the connection only, so the
    /// macro fits. Unlike the deferred user mail-spam/export/lists surfaces, the
    /// forwarder nest handlers exist, so the page is genuinely green.
    WasmForwarderMachine,
    ForwarderMachine,
    ForwarderAction,
    build_forwarders_machine,
);

wasm_admin_machine!(
    /// Web user-facing `mail-aliases` page — a person manages **their own**
    /// per-account mail addresses (exact / wildcard / disposable), each with a
    /// label, spam-threshold / rate-limit override, and disable / revoke /
    /// delete controls. Drives the User-class `MailAccountClient`
    /// (`fauna.bridges.{list,create,update,revoke,delete}_account_alias` +
    /// `generate_disposable_alias`). The wasm twin of the UniFFI exposure; same
    /// `snapshot`/`hydrate`/`dispatch` surface as the four admin wrappers (its
    /// builder takes only the connection, so the macro fits).
    /// See `docs/goal/behavior/mail-aliases.md` § Aliases UX.
    WasmMailAliasesMachine,
    MailAliasesMachine,
    MailAliasesAction,
    build_mail_aliases_machine,
);

/// Web user-facing `mail-spam` page (a sub-section of the mail-settings family) —
/// a person manages **their own** per-account spam classifier: reset the training
/// model, opt in/out of the deployment baseline, and undo individual training
/// events from the history. Same `snapshot`/`hydrate`/`dispatch` surface as the
/// macro wrappers, but a **bespoke** (non-macro) wrapper because undo of a
/// **client-written (sealed)** training row runs the reseal loop, which needs the
/// actor's MSEK — so the builder takes the actor `secret` + the deployment
/// `node_url` (to build the `MailSettingsMachine` sealed-model writer), exactly
/// like `WasmMailSettingsMachine`'s extra-arg shape, which the one-arg
/// `wasm_admin_machine!` macro can't express (`mail-spam.md` § Encrypted-mode
/// interaction). A server-written (plaintext) row still undoes via the server
/// path. See `docs/goal/behavior/mail-spam.md`.
#[wasm_bindgen]
pub struct WasmMailSpamMachine {
    inner: Rc<MailSpamMachine>,
}

impl WasmMailSpamMachine {
    /// Build over the SPA's browser WS-RPC client. Called by the
    /// `WsRpcClient::mailSpamMachine` factory in `src/rpc.rs`.
    pub(crate) fn build(
        client: InnerClient,
        secret: Vec<u8>,
        node_url: String,
    ) -> Result<Self, JsValue> {
        let arr: [u8; 32] = secret
            .try_into()
            .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
        let keypair = ActorKeypair::from_secret(arr);
        Ok(Self {
            inner: Rc::new(build_mail_spam_machine(
                client,
                keypair,
                crate::account_runtime::mail_store(),
                &node_url,
                crate::account_runtime::ledger_seam(),
            )),
        })
    }
}

#[wasm_bindgen]
impl WasmMailSpamMachine {
    /// The rendered snapshot as a plain JS object (sync).
    #[wasm_bindgen(js_name = snapshot)]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        to_js(&self.inner.snapshot())
    }

    /// Initial page load. Resolves `undefined`; read state via `snapshot()`.
    #[wasm_bindgen(js_name = hydrate)]
    pub fn hydrate(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            m.hydrate().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Dispatch an action (a JS object decoded into `MailSpamAction`). Resolves
    /// `undefined` on success; read state via `snapshot()`.
    #[wasm_bindgen(js_name = dispatch)]
    pub fn dispatch(&self, action: JsValue) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let action: MailSpamAction = from_js(action)?;
            m.dispatch(action).await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }
}

/// Web user-facing `mail-export` page (a sub-section of the mail-settings
/// family) — a person exports **their own** whole mailbox in a standard format
/// (mbox / Maildir++ / EML-zip) over a resumable wizard (format → scope →
/// confirm → progress → done). Drives the User-class `MailExportMachine`; the
/// wizard's step nav + scope edits are client-side, only
/// `Start`/`Pause`/`Resume`/`Cancel`/`Download`/`Discard` touch the nest.
///
/// Bespoke rather than `wasm_admin_machine!`, for two reasons the one-arg macro
/// can't express. The machine has **key custody** — the run mints and wraps a
/// per-session key and opens every record under the account's MSEK — so the
/// builder takes the actor `secret` + the deployment `node_url`, as
/// `WasmMailSpamMachine` does, plus the user's `handle` (the archive's root
/// directory) and the page's save port (§ Download flow's browser half,
/// `mail_export_delivery.rs`). And it exposes the two calls custody obliges
/// the page to make: `runExport` (spawned after a `Start`/`Resume` that lands
/// `Running` — custody without the spawn would open a session nothing drives)
/// and `setActorHandle`. See `docs/goal/behavior/mail-export.md`.
#[wasm_bindgen]
pub struct WasmMailExportMachine {
    inner: Rc<MailExportMachine>,
}

impl WasmMailExportMachine {
    /// Build over the SPA's browser WS-RPC client. Called by the
    /// `WsRpcClient::mailExportMachine` factory in `src/rpc.rs`.
    pub(crate) fn build(
        client: InnerClient,
        secret: Vec<u8>,
        node_url: String,
        handle: String,
        save_port: MailExportSavePort,
    ) -> Result<Self, JsValue> {
        let arr: [u8; 32] = secret
            .try_into()
            .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
        let keypair = ActorKeypair::from_secret(arr);
        let delivery = Arc::new(WebExportArchiveDelivery::new(client.clone(), save_port));
        Ok(Self {
            inner: Rc::new(build_mail_export_machine(
                client,
                keypair,
                crate::account_runtime::mail_store(),
                &node_url,
                &handle,
                delivery,
            )),
        })
    }
}

#[wasm_bindgen]
impl WasmMailExportMachine {
    /// The rendered snapshot as a plain JS object (sync).
    #[wasm_bindgen(js_name = snapshot)]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        to_js(&self.inner.snapshot())
    }

    /// Initial page load. Resolves `undefined`; read state via `snapshot()`.
    #[wasm_bindgen(js_name = hydrate)]
    pub fn hydrate(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            m.hydrate().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Dispatch an action (a JS object decoded into `MailExportAction`).
    /// Resolves `undefined` on success; read state via `snapshot()`.
    #[wasm_bindgen(js_name = dispatch)]
    pub fn dispatch(&self, action: JsValue) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let action: MailExportAction = from_js(action)?;
            m.dispatch(action).await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The chunk-relay drive loop (`MailExportMachine::run_export`). The page
    /// calls it once after each `Start`/`Resume` that lands a `Running`
    /// session and does not await it: the loop mutates the machine's own
    /// snapshot as it goes (a failure lands on the snapshot's `error`), and the
    /// page's progress tick is what repaints it.
    #[wasm_bindgen(js_name = runExport)]
    pub fn run_export(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            m.run_export().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Refresh the user's handle — the name of the archive's root directory
    /// and of the saved file. The page calls it at the gesture, before a
    /// `Start`, `Resume` or `Download`, since the handle can arrive or change
    /// after the machine is built.
    #[wasm_bindgen(js_name = setActorHandle)]
    pub fn set_actor_handle(&self, handle: String) {
        self.inner.set_actor_handle(handle);
    }
}

wasm_admin_machine!(
    /// Web user-facing `mail-import` page — the export twin's mirror image: a
    /// person pulls their existing mail from a foreign IMAP server (Gmail /
    /// Outlook / iCloud / generic) into their Fauna mailbox over a five-screen
    /// wizard (source → scope → confirm → progress → done). Drives the
    /// User-class `MailImportMachine`; owner-scoped, so the builder takes only
    /// the connection (the macro fits).
    ///
    /// ⚠ **On web this machine has one real seam and one stub**, and the split
    /// is the opposite of the export twin's. `MailImportNest` is REAL here
    /// (`MailImportClient<WsRpcClient>`, wasm-clean) — so hydrate, the session
    /// list, and pause/resume/cancel on an already-open session all work
    /// against the nest. `ImportSourceNest` is the stub: the web IMAP transport
    /// (sans-io `rustls` over the relay) is unbuilt and the relay host is
    /// undeployed, so `Connect` returns the honest `Rejected`
    /// (`mailbox-migration.md` § Implementation status today — the last unbuilt
    /// leg). The page therefore ships the reduced slice and says so, rather
    /// than hiding a control that cannot work. See `mailbox-migration.md`.
    WasmMailImportMachine,
    MailImportMachine,
    MailImportAction,
    build_mail_import_machine,
);

wasm_admin_machine!(
    /// Web user-facing `mail-lists` page (a sub-section of the mail-settings
    /// family) — a person manages **their own** per-account mailing lists (a list
    /// is a sixth alias kind): create / edit / delete a list, each with a friendly
    /// name, a send-from address on one of the user's domains, optional List-Help /
    /// List-Archive URLs, and a per-send recipient cap. Drives the User-class
    /// `MailListsMachine` (`fauna.bridges.{list,create,update,delete}_account_list`).
    /// Owner-scoped, so the builder takes only the connection (the macro fits). The
    /// list backend is unbuilt (`mail-mass-mailing.md` § Implementation status
    /// today), so the page surfaces the honest "unimplemented" rejection until it
    /// lands — never faked. The per-list members drill-down is the bespoke
    /// `WasmMailListMembersMachine` below. See `docs/goal/behavior/mail-mass-mailing.md`.
    WasmMailListsMachine,
    MailListsMachine,
    MailListsAction,
    build_mail_lists_machine,
);

/// Web user-facing `mail-list-members` page (the per-list members drill-down off
/// `mail-lists`) — add / batch-import / unsubscribe / resubscribe the members of
/// one list. Same `snapshot`/`hydrate`/`dispatch` surface as the macro wrappers,
/// but a bespoke (non-macro) wrapper because the builder is scoped to one list:
/// it takes `list_id_hex` + `list_name` and returns `Result` (a malformed hex id
/// is an error), which the one-arg `wasm_admin_machine!` macro can't express —
/// exactly like `WasmMailSettingsMachine`'s extra-arg shape. Owner-scoped (the
/// nest derives the actor from the authenticated caller). The list backend is
/// unbuilt (`mail-mass-mailing.md` § Implementation status today), so the page
/// surfaces the honest rejection on hydrate until it lands — never faked.
#[wasm_bindgen]
pub struct WasmMailListMembersMachine {
    inner: Rc<MailListMembersMachine>,
}

impl WasmMailListMembersMachine {
    /// Build over the SPA's browser WS-RPC client, scoped to one list. Called by
    /// the `WsRpcClient::mailListMembersMachine` factory in `src/rpc.rs`. Errors
    /// only on a malformed `list_id_hex`.
    pub(crate) fn build(
        client: InnerClient,
        list_id_hex: String,
        list_name: String,
    ) -> Result<Self, JsValue> {
        let machine =
            build_mail_list_members_machine(client, list_id_hex, list_name).map_err(err_to_js)?;
        Ok(Self {
            inner: Rc::new(machine),
        })
    }
}

#[wasm_bindgen]
impl WasmMailListMembersMachine {
    /// The rendered snapshot as a plain JS object (sync).
    #[wasm_bindgen(js_name = snapshot)]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        to_js(&self.inner.snapshot())
    }

    /// Initial page load. Resolves `undefined`; read state via `snapshot()`.
    #[wasm_bindgen(js_name = hydrate)]
    pub fn hydrate(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            m.hydrate().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Dispatch an action (a JS object decoded into `MailListMembersAction`).
    /// Resolves `undefined` on success; read state via `snapshot()`.
    #[wasm_bindgen(js_name = dispatch)]
    pub fn dispatch(&self, action: JsValue) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let action: MailListMembersAction = from_js(action)?;
            m.dispatch(action).await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }
}

/// Web `mail-settings` page — the user-facing mail-credential lifecycle (enable
/// mail, add/revoke credentials, rotate keys). Same `snapshot`/`hydrate`/`dispatch`
/// surface as the four admin wrappers, but a bespoke (non-macro) wrapper because
/// the builder also takes the actor `secret` (it signs submission tokens + derives
/// the account's mail custody) and the deployment `node_url` (MUA connection details) — the
/// `wasm_admin_machine!` macro only fits the one-arg `build(client)` shape.
#[wasm_bindgen]
pub struct WasmMailSettingsMachine {
    inner: Rc<MailSettingsMachine>,
}

impl WasmMailSettingsMachine {
    /// Build over the SPA's browser WS-RPC client. Called by the
    /// `WsRpcClient::mailSettingsMachine` factory in `src/rpc.rs`.
    pub(crate) fn build(
        client: InnerClient,
        secret: Vec<u8>,
        node_url: String,
    ) -> Result<Self, JsValue> {
        let arr: [u8; 32] = secret
            .try_into()
            .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
        let keypair = ActorKeypair::from_secret(arr);
        let folder_keys = crate::account_runtime::folder_key_store(keypair.actor_id_hex());
        Ok(Self {
            inner: Rc::new(build_mail_settings_machine(
                client,
                keypair,
                crate::account_runtime::mail_store(),
                &node_url,
                crate::account_runtime::ledger_seam(),
                Some(std::sync::Arc::new(
                    fauna_client_folders::CustodyServedSets(folder_keys),
                )),
            )),
        })
    }
}

/// Run the shared post-claim serving enablement over the SPA's browser client —
/// the body behind `WsRpcClient::applyPostClaimServingEnablement`.
pub(crate) fn apply_post_claim_serving_enablement(
    client: InnerClient,
    secret: Vec<u8>,
    node_url: String,
    intents: fauna_client_mail_settings::serving_enablement::ServingEnablementIntents,
) -> Result<js_sys::Promise, JsValue> {
    let arr: [u8; 32] = secret
        .try_into()
        .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
    let keypair = ActorKeypair::from_secret(arr);
    Ok(future_to_promise(async move {
        fauna_client_mail_settings::rpc_glue::dispatch_post_claim_serving_enablement(
            client,
            keypair,
            crate::account_runtime::mail_store(),
            node_url,
            crate::account_runtime::ledger_seam(),
            intents,
        )
        .await;
        Ok(JsValue::UNDEFINED)
    }))
}

/// `fauna_e2e_agent::SERVING_ENABLEMENT_KEY`'s value as JSON text — the web
/// automation bridge re-parses it, so the shape is the shared derivation's
/// (`serving_enablement::serving_enablement_json`), never re-built in TS.
#[wasm_bindgen(js_name = servingEnablementJson)]
pub fn serving_enablement_json() -> String {
    fauna_client_mail_settings::serving_enablement::serving_enablement_json_text()
}

#[wasm_bindgen]
impl WasmMailSettingsMachine {
    /// The rendered snapshot as a plain JS object (sync).
    #[wasm_bindgen(js_name = snapshot)]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        to_js(&self.inner.snapshot())
    }

    /// Initial page load. Resolves `undefined`; read state via `snapshot()`.
    #[wasm_bindgen(js_name = hydrate)]
    pub fn hydrate(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            m.hydrate().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Opportunistically refresh the published mail content-sealing epoch
    /// schedule on every successful (re)connect
    /// (`docs/goal/architecture/encryption-at-rest.md` § Capability tiering →
    /// *Content-sealing epochs*), mirroring the native apps' posture
    /// (linux `FaunaClient::refresh_mail_epoch_schedule`, fired at the same
    /// universal post-auth hook as the deployment-seed custody leg,
    /// best-effort/log-only).
    /// Delegates to the shared, idempotent
    /// `MailSettingsMachine::refresh_epoch_schedule`, which no-ops when mail
    /// isn't enabled (no MSEK) — always safe to call unconditionally. Resolves
    /// `undefined`; the caller should treat a rejection as best-effort
    /// (re-converges next connect) and never surface it to the user.
    #[wasm_bindgen(js_name = refreshEpochSchedule)]
    pub fn refresh_epoch_schedule(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            m.refresh_epoch_schedule().await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Dispatch an action (a JS object decoded into `MailSettingsAction`).
    /// Resolves `undefined` on success; read state via `snapshot()`.
    #[wasm_bindgen(js_name = dispatch)]
    pub fn dispatch(&self, action: JsValue) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let action: MailSettingsAction = from_js(action)?;
            m.dispatch(action).await.map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The account's complete **standing** recipient-mail key set for the
    /// conversations SMTP receive rail, as a flat `Uint8Array` of
    /// `k × (32 + 2400)` bytes — `x25519_secret ∥ mlkem_dk` per generation, the
    /// current MSEK's first, then one per prior grace generation. Empty when
    /// mail isn't enabled. The web inbound-mail poll feeds it to
    /// `WasmConversationsManager.setRecipientKeypairs` so sealed inbound records
    /// open client-side under the same key set the MDA holds (the MSEK itself
    /// never crosses into JS — only the derived, generation-scoped secrets). See
    /// `MailSettingsMachine::standing_mail_keypairs`.
    #[wasm_bindgen(js_name = recipientStandingKeypairs)]
    pub fn recipient_standing_keypairs(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let keypairs = m.standing_mail_keypairs().await.map_err(err_to_js)?;
            const LEN: usize = 32 + fauna_mls::wrapped_blob::MLKEM768_DECAPS_KEY_LEN;
            let mut flat = Vec::with_capacity(keypairs.len() * LEN);
            for kp in &keypairs {
                flat.extend_from_slice(&kp.x25519_secret[..]);
                match kp.mlkem_dk.as_deref() {
                    Some(dk) => flat.extend_from_slice(dk),
                    // A derived keypair always carries the ML-KEM half; the
                    // boundary shape is fixed-width, so refuse rather than
                    // misalign the reader.
                    None => return Err(JsValue::from_str("standing keypair without ML-KEM half")),
                }
            }
            Ok(js_sys::Uint8Array::from(&flat[..]).into())
        })
    }

    /// The account's own X-Wing recipient **public** key as a `Uint8Array`
    /// (1216 B), empty when mail isn't enabled. The web receive rail sets it on
    /// `WasmConversationsManager` (`setRecipientPublicKey`) beside the standing
    /// key set, so the bridged rail can seal the user's own copy of a message
    /// it sends. See `MailSettingsMachine::recipient_public_key`.
    #[wasm_bindgen(js_name = recipientPublicKey)]
    pub fn recipient_public_key(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let public = m.recipient_public_key().await.map_err(err_to_js)?;
            let bytes = public.map(|p| p.to_bytes().to_vec()).unwrap_or_default();
            Ok(js_sys::Uint8Array::from(&bytes[..]).into())
        })
    }

    /// The client's **mail-epoch roots** (content-sealing-epochs opener chain,
    /// design § 4/§ 5) as a flat `Uint8Array` of `N × 32` bytes — the current
    /// MSEK's root then one per prior grace generation. The web receive rail
    /// sets these on `WasmConversationsManager` (`setMailEpochRoots`) alongside
    /// the recipient secret so it opens mail sealed under a mail epoch key once
    /// the write flip is thrown. Empty array when mail isn't enabled. The MSEK
    /// never crosses into JS — only the derived roots. See
    /// `MailSettingsMachine::mail_epoch_roots`.
    #[wasm_bindgen(js_name = mailEpochRoots)]
    pub fn mail_epoch_roots(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let roots = m.mail_epoch_roots().await.map_err(err_to_js)?;
            Ok(js_sys::Uint8Array::from(&roots[..]).into())
        })
    }

    /// Whether a spam-model write would take the **sealed client-side** path —
    /// the fetch-avoidance pre-check before paying for a train-text fetch
    /// (`fauna.posts.get`). Resolves a `boolean`. The WASM twin of
    /// `MailSettingsMachine::sealed_spam_write_available` (reads the
    /// `spam-model-sealed-at-rest` feature-presence token + MSEK presence).
    #[wasm_bindgen(js_name = sealedSpamWriteAvailable)]
    pub fn sealed_spam_write_available(&self) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let available = m.sealed_spam_write_available().await.map_err(err_to_js)?;
            Ok(JsValue::from_bool(available))
        })
    }

    /// Apply one **train** event to the caller's per-user spam model on the
    /// client — fetch sealed → unwrap → train on `text` → re-seal
    /// (hybrid-aware) → write back opaque via `fauna.bridges.put_spam_model`.
    /// The WASM twin of `MailSettingsMachine::train_spam_model_client` (the
    /// tier-1 model-write leg, `mail-spam.md` § Encrypted-mode interaction).
    /// Resolves `{ sealed: boolean, sample_count: number }`; `sealed == false`
    /// ⇒ no sealed write was possible (mail not enabled) and nothing was
    /// written. A moderation-queue *server* row's correction uses
    /// `trainModerationCorrection` instead (it also runs the nest half).
    #[wasm_bindgen(js_name = trainSpamModelClient)]
    pub fn train_spam_model_client(&self, text: String, is_spam: bool) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let outcome = m
                .train_spam_model_client(text, is_spam)
                .await
                .map_err(err_to_js)?;
            to_js(&outcome)
        })
    }

    /// Apply one **mail-surface train** event — the live `Insert` consumer, the
    /// WASM twin of `MailSettingsMachine::train_spam_model_client_mail`. A
    /// "mark as spam" / "mark as not spam" gesture on a conversation/mail message
    /// carries the message's `message_id` (opaque bytes) + `mailbox` + `subject`
    /// so the client-side train **also seals an audit row** (`SpamHistoryOp::Insert`)
    /// atomically with the model re-seal — unlike `trainSpamModelClient` (the
    /// model-only social train). Resolves `{ sealed: boolean, sample_count: number }`;
    /// `sealed == false` ⇒ no sealed write was possible (mail not enabled) and
    /// nothing was written — there is no server-side train to fall back to.
    #[wasm_bindgen(js_name = trainSpamModelClientMail)]
    pub fn train_spam_model_client_mail(
        &self,
        text: String,
        is_spam: bool,
        message_id: Vec<u8>,
        mailbox: String,
        subject: String,
    ) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let outcome = m
                .train_spam_model_client_mail(text, is_spam, message_id, mailbox, subject)
                .await
                .map_err(err_to_js)?;
            to_js(&outcome)
        })
    }

    /// The moderation-queue **training correction** for a server row — the WASM
    /// twin of `MailSettingsMachine::train_moderation_correction`, the one shared
    /// flow: when a sealed write is possible, `fauna.posts.get` → train the sealed
    /// model client-side; then ALWAYS `fauna.moderation.train` (the read gate +
    /// report capture), whose error is the flow's error. Resolves
    /// `{ sealed: boolean, sample_count: number }` (`sealed == false` ⇒ the model
    /// half was skipped).
    #[wasm_bindgen(js_name = trainModerationCorrection)]
    pub fn train_moderation_correction(
        &self,
        content_id: String,
        is_spam: bool,
    ) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let outcome = m
                .train_moderation_correction(content_id, is_spam)
                .await
                .map_err(err_to_js)?;
            to_js(&outcome)
        })
    }

    /// Reveal a single credential's secret (password / OAUTHBEARER token) on
    /// demand, for the `mail-settings-credential-item-reveal-secret` toggle. A
    /// pure read from the client's own `fauna.state.mail` custody (sealed under `BackupKey`),
    /// so the secret is recoverable without a revoke + re-add. Resolves the plain
    /// secret `string` (shown for the user to copy into their MUA) — the WASM twin
    /// of the `MailSettingsMachine::reveal_credential_secret` UniFFI export,
    /// converting `SecretString` → plain `String` exactly as the native FFI
    /// custom-type marshalling does. Rejects `UnknownCredential` if no row matches.
    /// See `docs/goal/behavior/mail-credentials.md` § Re-reveal a credential secret.
    #[wasm_bindgen(js_name = revealCredentialSecret)]
    pub fn reveal_credential_secret(&self, credential_id: String) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let secret = m
                .reveal_credential_secret(credential_id)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_str(secret.as_str()))
        })
    }

    /// Enable mail for this actor with a **freshly generated** strong password,
    /// resolving that password `string` **once** (one-time reveal). The WASM twin
    /// of the `MailSettingsMachine::enable_mail_with_generated_password` UniFFI
    /// export — the shared "auto-mint mailbox with a generated password" path the
    /// onboarding auto-complete launch glue (`onboarding.md` § Enable-email at
    /// claim) calls instead of a bare `set_mail_enabled`, so the box gets a working
    /// mailbox with no hand-entered password. It runs the same `enable_mail` mint
    /// (MSEK + recipient pubkey + wrapped-MSEK + submission token + credential) and
    /// fires the deployment-wide Admin-class `set_mail_enabled(true)` as its final
    /// step (booting the co-located bridge, auto-approved nest-side). The web glue
    /// drops the returned password — it is sealed into the credential and
    /// re-revealable via `revealCredentialSecret` on the mail-settings page. Rejects
    /// `InvalidState` if mail is already enabled (the idempotency guard).
    #[wasm_bindgen(js_name = enableMailWithGeneratedPassword)]
    pub fn enable_mail_with_generated_password(&self, display_name: String) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let password = m
                .enable_mail_with_generated_password(display_name)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_str(password.as_str()))
        })
    }

    /// Auto-enable mail for a freshly-registered **non-admin** user at their first
    /// authenticated client setup. The WASM twin of
    /// `MailSettingsMachine::auto_enable_mail_for_new_user` — mints a mailbox with a
    /// generated password **iff** the deployment has mail on
    /// (`deployment_mail_enabled`), the auto-enable-for-new-users policy is on
    /// (`auto_enable_policy` — both read from `fauna.setup.status`), and the actor
    /// has no mailbox yet. Resolves to the generated password `string` (the one-time
    /// reveal) on a mint, or `null` — **not** a reject — when a gate fails (policy
    /// off, or a mailbox already exists), so the web first-setup glue can call it
    /// unconditionally on the non-admin path. The web glue drops the password — it is
    /// sealed into the credential and re-revealable via `revealCredentialSecret`.
    /// `mail-credentials.md` § Auto-enable for new users.
    #[wasm_bindgen(js_name = autoEnableMailForNewUser)]
    pub fn auto_enable_mail_for_new_user(
        &self,
        deployment_mail_enabled: bool,
        auto_enable_policy: bool,
        display_name: String,
    ) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let password = m
                .auto_enable_mail_for_new_user(
                    deployment_mail_enabled,
                    auto_enable_policy,
                    display_name,
                )
                .await
                .map_err(err_to_js)?;
            Ok(match password {
                Some(p) => JsValue::from_str(p.as_str()),
                None => JsValue::NULL,
            })
        })
    }

    /// Enable a **CalDAV mailbox** for this actor with a **freshly generated**
    /// strong password, resolving that password `string` **once** (one-time
    /// reveal). The WASM twin of the
    /// `MailSettingsMachine::enable_caldav_mailbox_with_generated_password` UniFFI
    /// export — the calendar twin of `enableMailWithGeneratedPassword` the web
    /// first-setup glue calls when the user enabled CalDAV but not email, so a
    /// CalDAV-only user gets the shared MSEK their calendar store seals under with
    /// no hand-entered password. It mints the read-only mailbox material (MSEK +
    /// recipient pubkey + wrapped-MSEK + credential — **no** submission token,
    /// **no** `set_mail_enabled` deployment flip), so the user stays genuinely
    /// mailbox-less. Idempotent: a no-op resolving the existing-credential password
    /// when the shared MSEK already exists (email or CalDAV already on), so the glue
    /// can call it unconditionally on a CalDAV-enable. The web glue drops the
    /// returned password — it is sealed into the credential and re-revealable via
    /// `revealCredentialSecret` on the mail-settings page.
    #[wasm_bindgen(js_name = enableCaldavMailboxWithGeneratedPassword)]
    pub fn enable_caldav_mailbox_with_generated_password(
        &self,
        display_name: String,
    ) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let password = m
                .enable_caldav_mailbox_with_generated_password(display_name)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_str(password.as_str()))
        })
    }

    /// Enable a **CalDAV mailbox** under a **caller-provided** PLAIN bridge
    /// password — the password twin of `enableCaldavMailboxWithGeneratedPassword`,
    /// the WASM twin of `MailSettingsMachine::enable_caldav_mailbox_with_password`.
    /// Resolves `undefined` on success. The shape a future "choose your own CalDAV
    /// password" web UI would call (and the e2e's known-password organizer mint).
    /// Idempotent: a no-op once the shared MSEK exists.
    #[wasm_bindgen(js_name = enableCaldavMailboxWithPassword)]
    pub fn enable_caldav_mailbox_with_password(
        &self,
        display_name: String,
        password: String,
    ) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            m.enable_caldav_mailbox_with_password(display_name, password)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Enable a **CardDAV mailbox** for this actor with a **freshly generated**
    /// strong password, resolving that password `string` **once** (one-time
    /// reveal). The WASM twin of the
    /// `MailSettingsMachine::enable_carddav_mailbox_with_generated_password`
    /// UniFFI export — the contacts twin of
    /// `enableCaldavMailboxWithGeneratedPassword`, called by the web first-setup
    /// glue when the user enabled CardDAV but neither email nor CalDAV, so a
    /// CardDAV-only user gets the shared MSEK their address-book store seals
    /// under with no hand-entered password. Same read-only recipe (no submission
    /// token, no deployment flip); idempotent — reduced to recording the
    /// per-actor `carddav_enabled` flag when the shared MSEK already exists, so
    /// the glue can call it unconditionally on a CardDAV-enable.
    #[wasm_bindgen(js_name = enableCarddavMailboxWithGeneratedPassword)]
    pub fn enable_carddav_mailbox_with_generated_password(
        &self,
        display_name: String,
    ) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            let password = m
                .enable_carddav_mailbox_with_generated_password(display_name)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::from_str(password.as_str()))
        })
    }

    /// Enable a **CardDAV mailbox** under a **caller-provided** PLAIN bridge
    /// password — the password twin of
    /// `enableCarddavMailboxWithGeneratedPassword`, the WASM twin of
    /// `MailSettingsMachine::enable_carddav_mailbox_with_password` (the e2e's
    /// known-password mint). Resolves `undefined` on success. Idempotent.
    #[wasm_bindgen(js_name = enableCarddavMailboxWithPassword)]
    pub fn enable_carddav_mailbox_with_password(
        &self,
        display_name: String,
        password: String,
    ) -> js_sys::Promise {
        let m = self.inner.clone();
        future_to_promise(async move {
            m.enable_carddav_mailbox_with_password(display_name, password)
                .await
                .map_err(err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }
}

// ── mail-add-credential password/token helpers ──────────────────────────────
//
// The WASM twin of the UniFFI exports in `fauna-ffi/src/mail_admin.rs`
// (`generate_bridge_password` / `generate_bridge_token` /
// `warn_manual_bridge_password`). Pure functions over
// `fauna_client_mail_settings::password_gen`, not machine methods — the
// `mail-add-credential` dialog mints a credential secret + decides the
// manual-password warning entirely from shared Rust (priority #2), so the web
// `MailSettingsSection` calls these instead of re-deriving the charset / token
// size / warning rule in TypeScript. The `SecretString` is converted to a plain
// `String` (shown-once for the user to copy into their MUA), exactly as the
// native FFI does. See `docs/goal/behavior/mail-credentials.md`
// § Auto-generated bridge password (PLAIN) + § OAUTHBEARER token issuer.

/// Generate a random `a-zA-Z0-9` bridge password (the "Auto-generate" default on
/// the `mail-add-credential` dialog, `mail-add-credential-autogenerate-toggle`).
#[wasm_bindgen(js_name = generateBridgePassword)]
pub fn generate_bridge_password() -> String {
    fauna_client_mail_settings::password_gen::generate_bridge_password().into()
}

/// Mint a fresh OAUTHBEARER credential token (32 random bytes → 64 lowercase hex
/// chars) for the `mail-add-credential` dialog's bearer-token kind. Replaces the
/// web's prior local 24-byte mint (a real under-spec divergence — the spec is 32
/// bytes / ≥128 bits).
#[wasm_bindgen(js_name = generateBridgeToken)]
pub fn generate_bridge_token() -> String {
    fauna_client_mail_settings::password_gen::generate_bridge_token().into()
}

/// Whether to show `mail-add-credential-weak-password-warning`: true only when
/// auto-generate is OFF on an encrypted nest (a plaintext nest's data already
/// rests unencrypted, so a weak password adds no incremental at-rest exposure).
#[wasm_bindgen(js_name = warnManualBridgePassword)]
pub fn warn_manual_bridge_password(auto_generate: bool, nest_encrypted: bool) -> bool {
    fauna_client_mail_settings::password_gen::warn_manual_password(auto_generate, nest_encrypted)
}

/// Resolve the PLAIN-form password field at a settled toggle edge (kind-select
/// → PLAIN, or the auto-generate toggle) — a freshly-minted secret when `kind`
/// is `"Plain"` and `autoGenerate` is on, `null` (clear the field for manual
/// entry) otherwise. `kind` is the serde variant-name string (`"Plain"` /
/// `"OAuthBearer"`), the same shape `credentialKindBadge` takes. **Call ONLY at
/// that settled edge, never again at submit** — store the returned value and
/// submit it verbatim, so the credential persisted is byte-identical to what
/// the user copied. See
/// `fauna_client_mail_settings::password_gen::resolve_autogenerated_password`.
#[wasm_bindgen(js_name = resolveAutogeneratedBridgePassword)]
pub fn resolve_autogenerated_bridge_password(
    kind: JsValue,
    auto_generate: bool,
) -> Result<JsValue, JsValue> {
    let kind: CredentialKind = from_js(kind)?;
    to_js(
        &fauna_client_mail_settings::password_gen::resolve_autogenerated_password(
            kind,
            auto_generate,
        )
        .map(String::from),
    )
}

/// `fauna_client_mail_settings::password_gen::password_strength_label` → a
/// `LocalizedText` `{ key, args }` (`settings.mail.strength_{weak,fair,strong}`)
/// the SPA resolves through `resolveLocalized`, or `null` for an empty password.
/// The advisory `mail-add-credential-password-strength-meter` readout: length-only
/// (charset is fixed), canonical `<8` Weak / `<16` Fair / `≥16` Strong — so web
/// stops hand-rolling the raw-English `>= 12 Strong / >= 8 Fair / Weak` meter and
/// gains i18n + the reference threshold. See mail-settings.md.
#[wasm_bindgen(js_name = passwordStrengthLabel)]
pub fn password_strength_label(password: &str) -> Result<JsValue, JsValue> {
    to_js(&fauna_client_mail_settings::password_gen::password_strength_label(password))
}

/// Substitute the logged-in `handle` into a `MailCredentialSummary.mua_username`
/// template (which carries `{handle}` as its only remaining placeholder), yielding
/// the exact `local@domain` a third-party MUA configures. The WASM twin of the
/// `resolve_mua_username` UniFFI export in `fauna-ffi/src/mail_admin.rs` — web's
/// `mail-settings` credential rows call it instead of hand-rolling a `replace`
/// that could diverge (priority #2). The bug-prone parts (the `+<credential_id>`
/// suffix, the `default`→bare rule, the domain) are already baked into the shared
/// `mua_username` field.
#[wasm_bindgen(js_name = resolveMuaUsername)]
pub fn resolve_mua_username(mua_username: String, handle: String) -> String {
    fauna_client_mail_settings::resolve_mua_username(&mua_username, &handle)
}
